//! Conversation desk. Owns Hermes commands and applies gateway events.
//!
//! The GPUI window reads this state and sends intents. It does not speak HTTP.

use std::collections::HashSet;

use crate::backend::{self, Backend};
use crate::model::*;
use crate::present::{self, Controls, QuestionDraft};

#[derive(Clone)]
struct PendingModel {
    runtime_id: String,
    target: ModelIdentity,
    prime: Option<ModelIdentity>,
}

pub struct ConfirmPrompt {
    pub message: String,
    pub(crate) target: ModelIdentity,
}

struct SwitchPlan {
    stored: String,
    runtime: String,
    prime: Option<ModelIdentity>,
}

struct Outgoing {
    id: String,
    runtime_id: String,
    text: String,
    busy: bool,
}

pub struct Desk {
    backend: Option<Backend>,
    pub state: AppState,
    pending_resumes: HashSet<String>,
    model_pending: Option<PendingModel>,
    select_created: bool,
    sequence: u64,
    pub follow_bottom: bool,
    pub sidebar_open: bool,
    pub picker_open: bool,
    pub picker_query: String,
    pub picker_index: usize,
    pub picker_note: String,
    pub confirm: Option<ConfirmPrompt>,
    pub questions: Vec<QuestionDraft>,
    question_key: Option<String>,
    pub form_note: String,
    pub draft_generation: u64,
}

impl Desk {
    pub fn new(config: Result<Config, String>) -> Self {
        let (backend, notice) = match config {
            Ok(config) => (Some(backend::spawn(config)), String::new()),
            Err(error) => (None, error),
        };
        let mut desk = Self::offline(notice);
        desk.backend = backend;
        desk
    }

    pub fn offline(notice: impl Into<String>) -> Self {
        Self {
            backend: None,
            state: AppState {
                notice: notice.into(),
                ..AppState::default()
            },
            pending_resumes: HashSet::new(),
            model_pending: None,
            select_created: false,
            sequence: 0,
            follow_bottom: true,
            sidebar_open: true,
            picker_open: false,
            picker_query: String::new(),
            picker_index: 0,
            picker_note: String::new(),
            confirm: None,
            questions: Vec::new(),
            question_key: None,
            form_note: String::new(),
            draft_generation: 0,
        }
    }

    pub fn shutdown(&self) {
        if let Some(backend) = &self.backend {
            let _ = backend.send(Command::Shutdown);
        }
    }

    pub fn poll(&mut self) -> bool {
        let events = self.drain_events();
        let changed = !events.is_empty();
        for event in events {
            self.apply(event);
        }
        changed
    }

    fn drain_events(&self) -> Vec<Event> {
        let Some(backend) = &self.backend else {
            return Vec::new();
        };
        let mut events = Vec::new();
        for _ in 0..512 {
            let Some(event) = backend.try_recv() else {
                break;
            };
            events.push(event);
        }
        events
    }

    pub fn controls(&self) -> Controls {
        present::controls(&self.state, &self.pending_resumes, self.backend.is_some())
    }

    pub fn connection(&self) -> present::ConnectionView {
        present::connection_view(self.state.connection.as_ref(), self.backend.is_some())
    }

    pub fn draft(&self) -> String {
        self.open_chat()
            .map(|chat| chat.draft.clone())
            .unwrap_or_default()
    }

    pub fn selected_revision(&self) -> u64 {
        self.open_chat().map(|chat| chat.revision).unwrap_or(0)
    }

    pub fn set_draft(&mut self, text: String) {
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        self.state.conversations.entry(id).or_default().draft = text;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    pub fn create(&mut self) -> bool {
        if !self.controls().new_chat {
            return false;
        }
        self.select_created = true;
        self.state.creating = true;
        if !self.command(Command::Create {
            title: "New conversation".into(),
        }) {
            self.state.creating = false;
            self.select_created = false;
            return false;
        }
        true
    }

    pub fn select(&mut self, id: &str) {
        self.state.selected = Some(id.to_string());
        self.state.notice.clear();
        self.follow_bottom = true;
        self.draft_generation = self.draft_generation.saturating_add(1);
        self.state.conversations.entry(id.to_string()).or_default();
        if self.needs_resume(id) {
            self.resume(id);
        }
        self.sync_questions();
    }

    pub fn reload(&mut self) {
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        self.resume(&id);
    }

    pub fn reconnect(&mut self) {
        self.state.notice.clear();
        self.state.disconnect();
        self.pending_resumes.clear();
        self.model_pending = None;
        self.confirm = None;
        let _ = self.command(Command::Reconnect);
        self.sync_questions();
    }

    pub fn stop(&mut self) {
        let Some(runtime_id) = self.runtime_of_selected() else {
            return;
        };
        let _ = self.command(Command::Interrupt { runtime_id });
    }

    pub fn load_more(&mut self) {
        if !self.controls().more_enabled {
            return;
        }
        let offset = self.state.sessions.len();
        self.state.list_loading = true;
        let _ = self.command(Command::List { offset });
    }

    pub fn load_older(&mut self) {
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        let Some(before) = self
            .state
            .conversations
            .get_mut(&id)
            .and_then(Conversation::begin_history_page)
        else {
            return;
        };
        self.follow_bottom = false;
        if !self.command(Command::History {
            stored_id: id.clone(),
            before,
        }) && let Some(conversation) = self.state.conversations.get_mut(&id)
        {
            conversation.history_loading = false;
        }
    }

    pub fn submit(&mut self) -> bool {
        if self.reloading() || !self.controls().send {
            return false;
        }
        let Some(outgoing) = self.outgoing() else {
            return false;
        };
        if outgoing.text.is_empty() {
            return false;
        }
        if outgoing.busy {
            self.steer(outgoing)
        } else {
            self.send_submit(outgoing)
        }
    }

    pub fn open_picker(&mut self) {
        if !self.controls().model_enabled {
            return;
        }
        self.picker_open = true;
        self.picker_query.clear();
        self.picker_index = 0;
        self.picker_note.clear();
    }

    pub fn close_picker(&mut self) {
        self.picker_open = false;
    }

    pub fn set_picker_query(&mut self, query: String) {
        if query == self.picker_query {
            return;
        }
        self.picker_query = query;
        self.picker_index = 0;
        self.picker_note.clear();
    }

    pub fn move_picker(&mut self, step: i32) {
        let count = self.picker_rows().len() as i32;
        if count == 0 {
            return;
        }
        let next = self.picker_index as i32 + step;
        self.picker_index = next.clamp(0, count - 1) as usize;
    }

    pub fn picker_rows(&self) -> Vec<ModelOption> {
        let catalog = &self.state.catalog;
        catalog
            .search(&self.picker_query)
            .into_iter()
            .filter_map(|index| catalog.options.get(index).cloned())
            .collect()
    }

    pub fn commit_picker(&mut self) -> bool {
        self.commit_picker_index(self.picker_index)
    }

    pub fn commit_picker_index(&mut self, index: usize) -> bool {
        let rows = self.picker_rows();
        let Some(option) = rows.get(index) else {
            return false;
        };
        if !option.selectable() {
            self.picker_note = "This catalog entry cannot be sent to the gateway.".into();
            return false;
        }
        let identity = option.identity.clone();
        self.picker_index = index;
        self.picker_open = false;
        self.choose_model(identity, false);
        true
    }

    pub fn answer_confirm(&mut self, accept: bool) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        if accept {
            self.choose_model(confirm.target, true);
        }
    }

    pub fn approve(&mut self, choice: &str) {
        let _ = self.respond(InteractionAnswer::Approval(choice.to_string()));
    }

    pub fn set_question_text(&mut self, qid: &str, text: String) {
        if let Some(question) = self.question_mut(qid)
            && question.locked.is_none()
            && !question.skipped
        {
            question.text = text;
        }
    }

    pub fn toggle_choice(&mut self, qid: &str, index: usize) {
        let Some(question) = self.question_mut(qid) else {
            return;
        };
        if question.locked.is_some() || question.skipped {
            return;
        }
        if question.multi {
            flip_choice(question, index);
        } else {
            select_only(question, index);
        }
    }

    pub fn toggle_skip(&mut self, qid: &str) {
        if let Some(question) = self.question_mut(qid)
            && question.locked.is_none()
        {
            question.skipped = !question.skipped;
        }
    }

    pub fn submit_questions(&mut self) -> bool {
        let Ok(answers) = present::collect_answers(&self.questions) else {
            self.form_note = "Answer each question, or skip it.".into();
            return false;
        };
        self.form_note.clear();
        self.respond(InteractionAnswer::Clarification(answers))
    }

    pub fn cancel_questions(&mut self) {
        let _ = self.respond(InteractionAnswer::Cancel);
    }

    pub fn sync_questions(&mut self) {
        let next = self.question_identity();
        if next == self.question_key {
            return;
        }
        self.question_key = next;
        self.questions = self.build_questions();
        self.form_note.clear();
    }

    pub fn apply(&mut self, event: Event) {
        if is_model_event(&event) {
            self.apply_model(event);
        } else if is_roster_event(&event) {
            self.apply_roster(event);
        } else if is_text_event(&event) {
            self.apply_text(event);
        } else if is_turn_event(&event) {
            self.apply_turn(event);
        } else {
            self.apply_failure(event);
        }
        self.sync_questions();
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Desk {
    fn command(&mut self, command: Command) -> bool {
        let result = self
            .backend
            .as_ref()
            .ok_or_else(|| {
                "Connection configuration is missing. Correct it and restart the application."
                    .to_string()
            })
            .and_then(|backend| backend.send(command));
        match result {
            Ok(()) => true,
            Err(error) => {
                self.state.notice = error;
                false
            }
        }
    }

    fn open_chat(&self) -> Option<&Conversation> {
        self.state
            .selected
            .as_ref()
            .and_then(|id| self.state.conversations.get(id))
    }

    fn needs_resume(&self, id: &str) -> bool {
        self.state.conversations.get(id).is_none_or(|chat| {
            !chat.loaded || (chat.runtime_id.is_none() && chat.read_only_reason.is_none())
        })
    }

    fn resume(&mut self, id: &str) {
        if !self.pending_resumes.insert(id.to_string()) {
            return;
        }
        self.follow_bottom = true;
        if !self.command(Command::Resume {
            stored_id: id.to_string(),
        }) {
            self.pending_resumes.remove(id);
        }
    }

    fn reloading(&self) -> bool {
        self.state
            .selected
            .as_ref()
            .is_some_and(|id| self.pending_resumes.contains(id))
    }

    fn runtime_of_selected(&self) -> Option<String> {
        self.open_chat().and_then(|chat| chat.runtime_id.clone())
    }

    fn outgoing(&self) -> Option<Outgoing> {
        let id = self.state.selected.clone()?;
        let chat = self.state.conversations.get(&id)?;
        Some(Outgoing {
            id,
            runtime_id: chat.runtime_id.clone()?,
            text: chat.draft.trim().to_string(),
            busy: chat.status.steerable(),
        })
    }

    fn send_submit(&mut self, outgoing: Outgoing) -> bool {
        if !self.command(Command::Submit {
            runtime_id: outgoing.runtime_id,
            text: outgoing.text.clone(),
        }) {
            return false;
        }
        self.record_outgoing(&outgoing.id, outgoing.text, "Sending…", true);
        true
    }

    fn steer(&mut self, outgoing: Outgoing) -> bool {
        if !self.command(Command::Steer {
            runtime_id: outgoing.runtime_id,
            text: outgoing.text.clone(),
        }) {
            return false;
        }
        self.record_outgoing(&outgoing.id, outgoing.text, "Steering…", false);
        true
    }

    fn record_outgoing(&mut self, id: &str, text: String, activity: &str, submitting: bool) {
        self.sequence = self.sequence.saturating_add(1);
        let message_id = format!("client-{}", self.sequence);
        if let Some(chat) = self.state.conversations.get_mut(id) {
            chat.record_message(ChatMessage {
                id: message_id,
                role: Role::User,
                text,
            });
            chat.draft.clear();
            chat.activity = activity.to_string();
            if submitting {
                chat.status = TurnStatus::Submitting;
            }
        }
        self.draft_generation = self.draft_generation.saturating_add(1);
        self.follow_bottom = true;
    }

    fn choose_model(&mut self, target: ModelIdentity, confirm: bool) {
        let Some(plan) = self.plan_switch() else {
            return;
        };
        if !self.command(Command::SetModel {
            runtime_id: plan.runtime.clone(),
            prime: plan.prime.clone(),
            target: target.clone(),
            confirm_expensive: confirm,
        }) {
            return;
        }
        if let Some(chat) = self.state.conversations.get_mut(&plan.stored) {
            chat.begin_model_switch(target.clone());
        }
        self.model_pending = Some(PendingModel {
            runtime_id: plan.runtime,
            target,
            prime: plan.prime,
        });
    }

    fn plan_switch(&self) -> Option<SwitchPlan> {
        let stored = self.state.selected.clone()?;
        let chat = self.state.conversations.get(&stored)?;
        if chat.read_only_reason.is_some() || chat.model.switching() {
            return None;
        }
        let runtime = chat.runtime_id.clone()?;
        let prime = if chat.messages.is_empty() {
            chat.model.confirmed().cloned()
        } else {
            None
        };
        Some(SwitchPlan {
            stored,
            runtime,
            prime,
        })
    }

    fn question_mut(&mut self, qid: &str) -> Option<&mut QuestionDraft> {
        self.questions
            .iter_mut()
            .find(|question| question.qid == qid)
    }

    fn respond(&mut self, answer: InteractionAnswer) -> bool {
        let Some((stored, runtime, request_id)) = self.open_response() else {
            return false;
        };
        if !self.command(Command::Respond {
            runtime_id: runtime,
            request_id,
            answer,
        }) {
            return false;
        }
        if let Some(chat) = self.state.conversations.get_mut(&stored) {
            chat.response_sent();
        }
        self.sync_questions();
        true
    }

    fn open_response(&self) -> Option<(String, String, String)> {
        let stored = self.state.selected.clone()?;
        let chat = self.state.conversations.get(&stored)?;
        let request = chat.interaction.as_ref()?;
        if !chat.accepts_response(&request.id) {
            return None;
        }
        Some((stored, chat.runtime_id.clone()?, request.id.clone()))
    }

    fn question_identity(&self) -> Option<String> {
        let id = self.state.selected.as_ref()?;
        let request = self.state.conversations.get(id)?.interaction.as_ref()?;
        Some(format!("{id}:{}", request.id))
    }

    fn build_questions(&self) -> Vec<QuestionDraft> {
        let Some(request) = self.open_chat().and_then(|chat| chat.interaction.clone()) else {
            return Vec::new();
        };
        let mut questions = Vec::new();
        for question in &request.questions {
            if questions
                .iter()
                .any(|slot: &QuestionDraft| slot.qid == question.qid)
            {
                continue;
            }
            questions.push(draft_question(
                question,
                request.answers.get(&question.qid).cloned(),
            ));
        }
        questions
    }

    fn with_runtime(&mut self, runtime_id: &str, apply: impl FnOnce(&mut Conversation)) {
        if let Some(chat) = self.state.conversation_for_runtime_mut(runtime_id) {
            apply(chat);
        }
    }

    fn apply_model(&mut self, event: Event) {
        match event {
            Event::Catalog { catalog } => self.state.catalog = catalog,
            Event::ModelAck {
                runtime_id,
                resolved,
                deferred,
            } => self.on_model_ack(runtime_id, resolved, deferred),
            Event::ModelRejected {
                runtime_id,
                message,
            } => self.on_model_rejected(runtime_id, message),
            Event::ModelConfirmation {
                runtime_id,
                message,
                ..
            } => self.on_model_confirmation(&runtime_id, message),
            Event::ModelAmbiguous { runtime_id } => self.on_model_ambiguous(runtime_id),
            Event::ModelObserved { runtime_id, model } => {
                self.on_model_observed(&runtime_id, &model)
            }
            _ => {}
        }
    }

    fn on_model_ack(&mut self, runtime_id: String, resolved: ModelIdentity, deferred: bool) {
        let primed = self
            .model_pending
            .as_ref()
            .is_some_and(|pending| pending.runtime_id == runtime_id && pending.prime.is_some());
        self.settle_model(&runtime_id, |chat| {
            chat.model_acknowledged(resolved, deferred, !primed);
        });
    }

    fn on_model_rejected(&mut self, runtime_id: String, message: String) {
        self.settle_model(&runtime_id, Conversation::model_rejected);
        self.state.notice = message;
    }

    fn on_model_confirmation(&mut self, runtime_id: &str, message: String) {
        let pending = self
            .model_pending
            .clone()
            .filter(|pending| pending.runtime_id == runtime_id);
        self.settle_model(runtime_id, Conversation::model_rejected);
        if let Some(pending) = pending {
            self.confirm = Some(ConfirmPrompt {
                message,
                target: pending.target,
            });
        }
    }

    fn on_model_ambiguous(&mut self, runtime_id: String) {
        self.settle_model(&runtime_id, Conversation::model_ambiguous);
        self.state.notice = "The model change was accepted for delivery but never answered. It stays unconfirmed and will not be retried.".into();
    }

    fn on_model_observed(&mut self, runtime_id: &str, model: &str) {
        let Some(identity) = self.state.catalog.identity_for_model(model) else {
            return;
        };
        self.settle_model(runtime_id, |chat| chat.observe_model(identity));
    }

    fn settle_model(&mut self, runtime_id: &str, update: impl FnOnce(&mut Conversation)) {
        self.model_pending = None;
        if let Some(chat) = self.state.conversation_for_runtime_mut(runtime_id) {
            update(chat);
        }
    }

    fn apply_roster(&mut self, event: Event) {
        match event {
            Event::Connection(connection) => self.on_connection(connection),
            Event::Sessions {
                sessions,
                offset,
                has_more,
            } => self.on_sessions(sessions, offset, has_more),
            Event::Created {
                session,
                runtime_id,
                model,
            } => self.on_created(session, runtime_id, model),
            Event::Resumed {
                stored_id,
                runtime_id,
                messages,
                before,
                read_only_reason,
                model,
            } => self.on_resumed(
                stored_id,
                runtime_id,
                messages,
                before,
                read_only_reason,
                model,
            ),
            Event::History {
                stored_id,
                messages,
                before,
                next_before,
            } => self.on_history(stored_id, messages, before, next_before),
            _ => {}
        }
    }

    fn on_connection(&mut self, connection: ConnectionState) {
        let connected = connection == ConnectionState::Connected;
        if !connected {
            self.state.disconnect();
            self.state.catalog = Catalog::default();
            self.pending_resumes.clear();
            self.model_pending = None;
            self.confirm = None;
        }
        self.state.connection = Some(connection);
        self.state.creating = false;
        self.state.list_loading = connected;
        let selected = self.state.selected.clone();
        if connected {
            let _ = self.command(Command::List { offset: 0 });
            if let Some(id) = selected {
                self.resume(&id);
            }
        }
    }

    fn on_sessions(&mut self, sessions: Vec<SessionSummary>, offset: usize, has_more: bool) {
        if offset == 0 {
            self.state.sessions.clear();
        }
        for session in sessions {
            self.state.upsert_session(session);
        }
        self.state.list_has_more = has_more;
        self.state.list_loading = false;
    }

    fn on_created(
        &mut self,
        session: SessionSummary,
        runtime_id: String,
        model: Option<ModelIdentity>,
    ) {
        let id = session.stored_id.clone();
        self.state.sessions.insert(0, session);
        self.state.conversations.insert(
            id.clone(),
            Conversation {
                runtime_id: Some(runtime_id),
                loaded: true,
                ..Conversation::default()
            },
        );
        if let Some(model) = model
            && let Some(chat) = self.state.conversations.get_mut(&id)
        {
            chat.confirm_model(model);
        }
        self.state.creating = false;
        if self.select_created {
            self.select_created = false;
            self.select(&id);
        }
    }

    fn on_resumed(
        &mut self,
        stored_id: String,
        runtime_id: Option<String>,
        messages: Vec<ChatMessage>,
        before: usize,
        read_only_reason: Option<String>,
        model: Option<ModelIdentity>,
    ) {
        self.pending_resumes.remove(&stored_id);
        let chat = self.state.conversations.entry(stored_id).or_default();
        chat.runtime_id = runtime_id;
        chat.messages = messages;
        chat.history_before = before;
        chat.history_loading = false;
        chat.loaded = true;
        chat.read_only_reason = read_only_reason;
        chat.status = TurnStatus::Idle;
        chat.activity.clear();
        chat.interaction = None;
        chat.model = model.map_or(ModelState::Unknown, ModelState::Confirmed);
        chat.revision = chat.revision.saturating_add(1);
        self.model_pending = None;
        self.confirm = None;
    }

    fn on_history(
        &mut self,
        stored_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        next_before: usize,
    ) {
        if let Some(chat) = self.state.conversations.get_mut(&stored_id) {
            chat.receive_history_page(before, next_before, messages);
        }
    }

    fn apply_text(&mut self, event: Event) {
        match event {
            Event::Submitted { runtime_id } => {
                self.with_runtime(&runtime_id, Conversation::submit_acknowledged);
            }
            Event::MessageDelta {
                runtime_id,
                message_id,
                delta,
            } => self.with_runtime(&runtime_id, |chat| chat.receive_delta(message_id, delta)),
            Event::MessageComplete {
                runtime_id,
                message,
                status,
            } => self.finish_turn(runtime_id, message, status),
            Event::Activity { runtime_id, text } => {
                self.with_runtime(&runtime_id, |chat| chat.activity = text);
            }
            _ => {}
        }
    }

    fn finish_turn(&mut self, runtime_id: String, message: ChatMessage, status: TurnStatus) {
        let activity = present::completion_activity(&status);
        self.with_runtime(&runtime_id, |chat| {
            chat.finish_message(message);
            chat.activity = activity;
            chat.status = status;
            chat.interaction = None;
        });
    }

    fn apply_turn(&mut self, event: Event) {
        match event {
            Event::Interrupting { runtime_id } => {
                self.with_runtime(&runtime_id, Conversation::begin_stopping);
            }
            Event::SteerAccepted {
                runtime_id,
                outcome,
            } => self.with_runtime(&runtime_id, |chat| chat.steer_acknowledged(outcome)),
            Event::TurnRolled { runtime_id } => {
                self.with_runtime(&runtime_id, Conversation::turn_rolled);
            }
            Event::Interaction {
                runtime_id,
                request,
            } => self.with_runtime(&runtime_id, |chat| {
                chat.receive_interaction(request);
            }),
            Event::InteractionCancelled {
                runtime_id,
                request_id,
            } => self.with_runtime(&runtime_id, |chat| chat.withdraw_interaction(&request_id)),
            _ => {}
        }
    }

    fn apply_failure(&mut self, event: Event) {
        match event {
            Event::Deleted { stored_id } => self.on_deleted(&stored_id),
            Event::Error {
                operation,
                session_id,
                message,
            } => self.on_error(operation, session_id, message),
            _ => {}
        }
    }

    fn on_deleted(&mut self, stored_id: &str) {
        self.state
            .sessions
            .retain(|session| session.stored_id != stored_id);
        self.state.conversations.remove(stored_id);
        if self.state.selected.as_deref() == Some(stored_id) {
            self.state.selected = None;
            self.draft_generation = self.draft_generation.saturating_add(1);
        }
    }

    fn on_error(&mut self, operation: String, session_id: Option<String>, message: String) {
        self.state.notice = format!("{operation}: {message}");
        self.state.creating = false;
        self.state.list_loading = false;
        let Some(id) = session_id else {
            return;
        };
        self.pending_resumes.remove(&id);
        let stored = self.stored_for(&id);
        if let Some(stored) = stored
            && let Some(chat) = self.state.conversations.get_mut(&stored)
        {
            chat.history_loading = false;
            chat.operation_failed(&operation, message);
        }
    }

    fn stored_for(&self, id: &str) -> Option<String> {
        if self.state.conversations.contains_key(id) {
            Some(id.to_string())
        } else {
            self.state.stored_for_runtime(id)
        }
    }
}

fn is_model_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Catalog { .. }
            | Event::ModelAck { .. }
            | Event::ModelRejected { .. }
            | Event::ModelConfirmation { .. }
            | Event::ModelAmbiguous { .. }
            | Event::ModelObserved { .. }
    )
}

fn is_roster_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Connection(_)
            | Event::Sessions { .. }
            | Event::Created { .. }
            | Event::Resumed { .. }
            | Event::History { .. }
    )
}

fn is_text_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Submitted { .. }
            | Event::MessageDelta { .. }
            | Event::MessageComplete { .. }
            | Event::Activity { .. }
    )
}

fn is_turn_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Interrupting { .. }
            | Event::SteerAccepted { .. }
            | Event::TurnRolled { .. }
            | Event::Interaction { .. }
            | Event::InteractionCancelled { .. }
    )
}

fn draft_question(
    question: &crate::model::ClarificationQuestion,
    locked: Option<Option<String>>,
) -> QuestionDraft {
    QuestionDraft {
        qid: question.qid.clone(),
        prompt: question.prompt.clone(),
        choices: question.choices.clone(),
        multi: question.multi_select,
        locked,
        skipped: false,
        text: String::new(),
        selected: vec![false; question.choices.len()],
    }
}

fn flip_choice(question: &mut QuestionDraft, index: usize) {
    if let Some(flag) = question.selected.get_mut(index) {
        *flag = !*flag;
    }
}

fn select_only(question: &mut QuestionDraft, index: usize) {
    for (slot, flag) in question.selected.iter_mut().enumerate() {
        *flag = slot == index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn created(model: Option<ModelIdentity>) -> Event {
        Event::Created {
            session: SessionSummary {
                stored_id: "chat".into(),
                title: "Chat".into(),
                source: "cli".into(),
                updated_at: String::new(),
                read_only_reason: None,
                message_count: 0,
            },
            runtime_id: "rt".into(),
            model,
        }
    }

    #[test]
    fn streamed_text_lands_on_the_runtime_conversation() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.apply(Event::MessageDelta {
            runtime_id: "rt".into(),
            message_id: "m1".into(),
            delta: "Hello".into(),
        });
        let chat = desk.state.conversations.get("chat").expect("chat");
        assert_eq!(chat.messages[0].text, "Hello");
        assert_eq!(chat.status, TurnStatus::Streaming);
        assert_eq!(desk.state.selected.as_deref(), Some("chat"));
    }

    #[test]
    fn completion_clears_activity_when_the_turn_finishes() {
        let mut desk = Desk::offline("");
        desk.apply(created(None));
        desk.apply(Event::MessageComplete {
            runtime_id: "rt".into(),
            message: ChatMessage {
                id: "m1".into(),
                role: Role::Assistant,
                text: "Done".into(),
            },
            status: TurnStatus::Complete,
        });
        let chat = desk.state.conversations.get("chat").expect("chat");
        assert_eq!(chat.messages[0].text, "Done");
        assert!(chat.activity.is_empty());
        assert_eq!(chat.status, TurnStatus::Complete);
    }

    #[test]
    fn confirmation_keeps_the_previous_model_until_accepted() {
        let mut desk = Desk::offline("");
        let current = ModelIdentity {
            provider: "custom:acme".into(),
            model: "one".into(),
        };
        let target = ModelIdentity {
            provider: "custom:acme".into(),
            model: "two".into(),
        };
        desk.apply(created(Some(current.clone())));
        desk.state
            .conversations
            .get_mut("chat")
            .expect("chat")
            .begin_model_switch(target.clone());
        desk.model_pending = Some(PendingModel {
            runtime_id: "rt".into(),
            target: target.clone(),
            prime: None,
        });
        desk.apply(Event::ModelConfirmation {
            runtime_id: "rt".into(),
            target: target.clone(),
            message: "This model is expensive.".into(),
        });
        assert_eq!(
            desk.state.conversations.get("chat").expect("chat").model,
            ModelState::Confirmed(current)
        );
        assert_eq!(
            desk.confirm.as_ref().map(|prompt| prompt.message.as_str()),
            Some("This model is expensive.")
        );
    }
}
