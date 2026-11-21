//! Conversation desk. Owns Hermes commands and applies gateway events.
//!
//! The GPUI window reads this state and sends intents. It does not speak HTTP.

use std::collections::HashSet;

use crate::backend::{self, Backend};
use crate::model::*;
use crate::present::{self, Controls, QuestionDraft};

#[derive(Clone)]
struct PendingModel {
    stored_id: String,
    runtime_id: String,
    target: ModelIdentity,
    prime: Option<ModelIdentity>,
}

pub struct ConfirmPrompt {
    pub message: String,
    stored_id: String,
    target: ModelIdentity,
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
    pub composer_follow: Option<ComposerFollow>,
    recall: PromptRecall,
    held_files: Option<HeldFiles>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerFollow {
    Attach,
    Workspace(Option<String>),
}

struct HeldFiles {
    stored_id: String,
    files: Vec<StagedImage>,
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
            composer_follow: None,
            recall: PromptRecall::default(),
            held_files: None,
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
        self.state.active_draft().to_string()
    }

    pub fn selected_revision(&self) -> u64 {
        self.open_chat().map(|chat| chat.revision).unwrap_or(0)
    }

    pub fn set_draft(&mut self, text: String) {
        self.state.set_active_draft(text);
    }

    pub fn replace_draft(&mut self, text: String) {
        self.set_draft(text);
        self.bump_draft();
    }

    pub fn toggle_inspector(&mut self) {
        self.state.inspector_open = !self.state.inspector_open;
    }

    pub fn open_inspector(&mut self, tab: InspectorTab) {
        self.state.inspector_open = true;
        self.state.inspector_tab = tab;
    }

    pub fn messages(&self) -> Vec<ChatMessage> {
        self.selected_messages()
    }

    pub fn side_questions(&self) -> Vec<SideQuestion> {
        self.open_chat()
            .map(|chat| chat.side_questions.clone())
            .unwrap_or_default()
    }

    pub fn reasoning_text(&self) -> String {
        self.open_chat()
            .map(|chat| chat.reasoning.clone())
            .unwrap_or_default()
    }

    pub fn tool_traces(&self) -> Vec<ToolTrace> {
        self.open_chat()
            .map(|chat| chat.tools.clone())
            .unwrap_or_default()
    }

    pub fn telemetry(&self) -> SessionTelemetry {
        self.open_chat()
            .map(|chat| chat.telemetry.clone())
            .unwrap_or_default()
    }

    pub fn can_compress(&self) -> bool {
        crate::inspector::can_compress_conversation(self.open_chat(), self.controls().editable)
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
            cwd: self.state.launch_cwd.clone(),
        }) {
            self.state.creating = false;
            self.select_created = false;
            return false;
        }
        true
    }

    pub fn select(&mut self, id: &str) {
        let switching = self.state.selected.as_deref() != Some(id);
        self.state.selected = Some(id.to_string());
        self.state.notice.clear();
        self.follow_bottom = true;
        self.recall = PromptRecall::default();
        self.draft_generation = self.draft_generation.saturating_add(1);
        self.state.conversations.entry(id.to_string()).or_default();
        if switching {
            self.rebind_find(id);
        }
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
        if self.reloading() {
            return false;
        }
        let intent = parse_composer_input(self.state.active_draft());
        self.dispatch_intent(intent)
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
            self.choose_for(&confirm.stored_id, confirm.target, true);
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
        } else if is_side_event(&event) {
            self.apply_side(event);
        } else if is_studio_event(&event) {
            self.apply_studio(event);
        } else if is_timeline_event(&event) {
            self.apply_timeline(event);
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
        let text = chat.draft.trim().to_string();
        if !outgoing_ready(chat, &text) {
            return None;
        }
        Some(Outgoing {
            id,
            runtime_id: chat.runtime_id.clone()?,
            text,
            busy: chat.status.steerable(),
        })
    }

    fn send_submit(&mut self, outgoing: Outgoing) -> bool {
        let attachments = self.state.take_conversation_attachments(&outgoing.id);
        let display = crate::studio::format_optimistic_prompt(&outgoing.text, &attachments);
        let activity = submit_activity(!attachments.is_empty());
        self.hold_files(&outgoing.id, attachments.clone());
        if !self.command(Command::Submit {
            runtime_id: outgoing.runtime_id,
            text: outgoing.text.clone(),
            attachments,
        }) {
            self.give_back_held();
            return false;
        }
        self.record_outgoing(&outgoing.id, display, activity, true);
        true
    }

    fn restore_attachments(&mut self, id: &str, attachments: Vec<StagedImage>) {
        if let Some(chat) = self.state.conversations.get_mut(id) {
            chat.attachments = attachments;
        }
    }

    fn hold_files(&mut self, stored_id: &str, files: Vec<StagedImage>) {
        if files.is_empty() {
            return;
        }
        self.held_files = Some(HeldFiles {
            stored_id: stored_id.to_string(),
            files,
        });
    }

    fn give_back_held(&mut self) {
        self.give_back_if(None);
    }

    fn give_back_if(&mut self, session_id: Option<&str>) {
        let Some(held) = self.held_files.take() else {
            return;
        };
        if !held_matches(&self.state, &held, session_id) {
            self.held_files = Some(held);
            return;
        }
        self.restore_attachments(&held.stored_id, held.files);
    }

    fn release_held(&mut self, runtime_id: &str) {
        let stored = self.stored_for(runtime_id);
        let matches = self
            .held_files
            .as_ref()
            .is_some_and(|held| stored.as_deref() == Some(held.stored_id.as_str()));
        if matches {
            self.held_files = None;
        }
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
        let Some(stored) = self.state.selected.clone() else {
            return;
        };
        self.choose_for(&stored, target, confirm);
    }

    fn choose_for(&mut self, stored: &str, target: ModelIdentity, confirm: bool) {
        let Some(plan) = self.plan_for(stored) else {
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
            stored_id: plan.stored,
            runtime_id: plan.runtime,
            target,
            prime: plan.prime,
        });
    }

    fn plan_for(&self, stored: &str) -> Option<SwitchPlan> {
        let chat = self.state.conversations.get(stored)?;
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
            stored: stored.to_string(),
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
                stored_id: pending.stored_id,
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
        if let Some(chat) = self.state.conversation_for_runtime_mut(runtime_id) {
            chat.observe_model(identity);
        }
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
            self.state.workspaces = WorkspaceCatalog::default();
            self.state.skills.clear();
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
            let _ = self.command(Command::FetchStudio);
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
        let bundle = self.seed_created(session, runtime_id.clone(), model);
        self.state.creating = false;
        if self.select_created {
            self.select_created = false;
            self.select(&id);
        }
        if let Some((prompt, attachments)) = bundle {
            self.send_initial(&id, runtime_id, prompt, attachments);
        }
    }

    fn seed_created(
        &mut self,
        session: SessionSummary,
        runtime_id: String,
        model: Option<ModelIdentity>,
    ) -> Option<(String, Vec<StagedImage>)> {
        let draft = self.state.take_launch_draft_for_blank_chat();
        let attachments = self.state.take_launch_attachments_for_blank_chat();
        let bundle = self.state.take_pending_launch_bundle();
        let cwd = self.state.launch_cwd.clone().unwrap_or_default();
        let id = session.stored_id.clone();
        self.state.sessions.insert(0, session);
        self.state.conversations.insert(
            id,
            created_conversation(runtime_id, draft, attachments, cwd, model),
        );
        bundle
    }

    fn send_initial(
        &mut self,
        id: &str,
        runtime_id: String,
        prompt: String,
        attachments: Vec<StagedImage>,
    ) {
        let display = crate::studio::format_optimistic_prompt(&prompt, &attachments);
        let activity = submit_activity(!attachments.is_empty());
        self.hold_files(id, attachments.clone());
        if !self.command(Command::Submit {
            runtime_id,
            text: prompt.clone(),
            attachments,
        }) {
            self.restore_initial(id, prompt, Vec::new());
            self.give_back_held();
            return;
        }
        self.record_outgoing(id, display, activity, true);
    }

    fn restore_initial(&mut self, id: &str, prompt: String, attachments: Vec<StagedImage>) {
        if let Some(chat) = self.state.conversations.get_mut(id) {
            chat.draft = prompt;
            chat.attachments = attachments;
        }
        self.draft_generation = self.draft_generation.saturating_add(1);
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
        self.fill_resumed(
            &stored_id,
            runtime_id,
            messages,
            before,
            read_only_reason,
            model,
        );
        self.drop_switch(&stored_id);
        self.sync_find(&stored_id);
    }

    fn fill_resumed(
        &mut self,
        stored_id: &str,
        runtime_id: Option<String>,
        messages: Vec<ChatMessage>,
        before: usize,
        read_only_reason: Option<String>,
        model: Option<ModelIdentity>,
    ) {
        let chat = self
            .state
            .conversations
            .entry(stored_id.to_string())
            .or_default();
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
    }

    fn drop_switch(&mut self, stored_id: &str) {
        if self
            .model_pending
            .as_ref()
            .is_some_and(|pending| pending.stored_id == stored_id)
        {
            self.model_pending = None;
        }
        if self
            .confirm
            .as_ref()
            .is_some_and(|prompt| prompt.stored_id == stored_id)
        {
            self.confirm = None;
        }
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
        self.sync_find(&stored_id);
    }

    fn apply_text(&mut self, event: Event) {
        match event {
            Event::Submitted { runtime_id } => {
                self.release_held(&runtime_id);
                self.with_runtime(&runtime_id, Conversation::submit_acknowledged);
            }
            Event::MessageDelta {
                runtime_id,
                message_id,
                delta,
            } => self.with_runtime(&runtime_id, |chat| chat.receive_delta(&message_id, delta)),
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
            Event::SessionTitle { session_id, title } => {
                let _ = self.state.apply_session_title(&session_id, &title);
            }
            Event::SearchResults {
                query,
                request_seq,
                hits,
            } => {
                let _ = self.state.apply_search_results(query, request_seq, hits);
            }
            Event::SessionMutated {
                stored_id,
                mutation,
            } => self.on_mutation(&stored_id, &mutation),
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
        if operation == "create" {
            self.state.cancel_pending_launch();
            self.select_created = false;
        }
        if matches!(operation.as_str(), "submit" | "gateway") {
            self.give_back_if(session_id.as_deref());
        }
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

impl Desk {
    fn dispatch_intent(&mut self, intent: ComposerIntent) -> bool {
        match intent {
            ComposerIntent::Empty => self.submit_staged_only(),
            ComposerIntent::Prompt(_) => self.submit_prompt(),
            ComposerIntent::SideQuestion(question) => self.ask_side(question),
            ComposerIntent::Branch(title) => self.branch_from(None, title),
            ComposerIntent::Rewind(mode) => self.rewind(mode),
            other => self.dispatch_command(other),
        }
    }

    fn dispatch_command(&mut self, intent: ComposerIntent) -> bool {
        match intent {
            ComposerIntent::FindInTranscript(query) => self.open_find(query.as_deref()),
            ComposerIntent::TogglePin => self.toggle_pin(),
            ComposerIntent::Rename(title) => self.rename_to(title),
            ComposerIntent::Archive => self.archive_current(),
            ComposerIntent::Compress => self.compress_current(),
            ComposerIntent::AttachImage => self.follow_attach(),
            ComposerIntent::PickWorkspace(query) => self.follow_workspace(query),
            ComposerIntent::Empty
            | ComposerIntent::Prompt(_)
            | ComposerIntent::SideQuestion(_)
            | ComposerIntent::Branch(_)
            | ComposerIntent::Rewind(_) => false,
        }
    }

    fn submit_staged_only(&mut self) -> bool {
        if !self.has_staged_files() {
            return false;
        }
        self.submit_prompt()
    }

    fn has_staged_files(&self) -> bool {
        if self.state.selected.is_none() {
            return !self.state.launch_attachments.is_empty();
        }
        self.open_chat()
            .is_some_and(|chat| !chat.attachments.is_empty())
    }

    fn follow_attach(&mut self) -> bool {
        self.clear_selected_draft();
        self.composer_follow = Some(ComposerFollow::Attach);
        true
    }

    fn follow_workspace(&mut self, query: Option<String>) -> bool {
        self.clear_selected_draft();
        if let Some(chosen) = unique_workspace(&self.state, query.as_deref()) {
            return self.choose_workspace(chosen);
        }
        self.composer_follow = Some(ComposerFollow::Workspace(query));
        true
    }

    fn submit_prompt(&mut self) -> bool {
        if self.state.selected.is_none() {
            return self.launch();
        }
        let Some(outgoing) = self.outgoing() else {
            return false;
        };
        if outgoing.busy {
            return self.steer_text(outgoing);
        }
        self.send_submit(outgoing)
    }

    fn steer_text(&mut self, outgoing: Outgoing) -> bool {
        if outgoing.text.is_empty() {
            return false;
        }
        self.steer(outgoing)
    }

    fn launch(&mut self) -> bool {
        let Some((title, _)) = self.state.begin_launch_submit() else {
            return false;
        };
        self.select_created = true;
        let cwd = self.state.launch_cwd.clone();
        if !self.command(Command::Create { title, cwd }) {
            self.state.cancel_pending_launch();
            self.select_created = false;
            return false;
        }
        true
    }

    pub fn recall(&mut self, back: bool) -> bool {
        let prompts = self.recall_prompts();
        let current = self.state.active_draft().to_string();
        let Some(text) = self.recall.step(&prompts, &current, back) else {
            return false;
        };
        self.state.set_active_draft(text);
        self.bump_draft();
        true
    }

    fn recall_prompts(&self) -> Vec<String> {
        self.open_chat()
            .map(|chat| PromptRecall::prompts(&chat.messages))
            .unwrap_or_default()
    }

    pub fn quote_into_draft(&mut self, index: usize) -> bool {
        let Some(text) = self.message_text(index) else {
            return false;
        };
        let quoted = present::quote_markdown(&text);
        if quoted.is_empty() {
            return false;
        }
        let next = join_quote(self.state.active_draft(), &quoted);
        self.state.set_active_draft(next);
        self.recall = PromptRecall::default();
        self.bump_draft();
        true
    }

    fn message_text(&self, index: usize) -> Option<String> {
        self.open_chat()
            .and_then(|chat| chat.messages.get(index))
            .map(|message| message.text.clone())
    }

    fn bump_draft(&mut self) {
        self.draft_generation = self.draft_generation.saturating_add(1);
    }

    pub fn open_find(&mut self, query: Option<&str>) -> bool {
        let messages = self.selected_messages();
        if self.state.selected.is_none() {
            return false;
        }
        let stored = query.unwrap_or("");
        self.state.transcript_find.open_with(stored, &messages);
        true
    }

    pub fn set_find_query(&mut self, query: String) {
        let messages = self.selected_messages();
        self.state.transcript_find.set_query(query, &messages);
    }

    pub fn step_find(&mut self, delta: isize) -> Option<usize> {
        self.state.transcript_find.step(delta)
    }

    pub fn close_find(&mut self) {
        self.state.transcript_find.close();
    }

    fn selected_messages(&self) -> Vec<ChatMessage> {
        self.open_chat()
            .map(|chat| chat.messages.clone())
            .unwrap_or_default()
    }

    fn rebind_find(&mut self, id: &str) {
        if self.state.pending_find_session.as_deref() != Some(id) {
            self.state.pending_find_query = None;
            self.state.pending_find_session = None;
        }
        if !self.state.transcript_find.open {
            return;
        }
        let messages = self
            .state
            .conversations
            .get(id)
            .map(|chat| chat.messages.clone())
            .unwrap_or_default();
        self.state.transcript_find.recompute(&messages);
    }

    fn sync_find(&mut self, stored_id: &str) {
        if self.state.selected.as_deref() != Some(stored_id) {
            return;
        }
        let messages = self.selected_messages();
        if self.parked_find(stored_id) {
            let query = self.state.pending_find_query.take().unwrap_or_default();
            self.state.pending_find_session = None;
            self.state.transcript_find.open_with(&query, &messages);
            return;
        }
        if self.state.transcript_find.open {
            self.state.transcript_find.recompute(&messages);
        }
    }

    fn parked_find(&self, stored_id: &str) -> bool {
        self.state.pending_find_query.is_some()
            && self.state.pending_find_session.as_deref() == Some(stored_id)
    }

    pub fn set_session_query(&mut self, query: String) {
        self.state.session_query = query;
        self.fire_search();
    }

    fn fire_search(&mut self) {
        let query = self.state.session_query.clone();
        let Some((request_seq, clean)) = self.state.begin_remote_search(&query) else {
            return;
        };
        if !self.command(Command::SearchSessions {
            query: clean,
            request_seq,
        }) {
            self.state.remote_search_loading = false;
        }
    }

    pub fn set_filter(&mut self, filter: SessionFilter) {
        self.state.session_filter = filter;
    }

    pub fn toggle_pin(&mut self) -> bool {
        let Some(stored_id) = self.connected_selection() else {
            return false;
        };
        let pinned = !self.state.is_selected_pinned();
        self.mutate(stored_id, SessionMutation::SetPinned(pinned))
    }

    pub fn rename_to(&mut self, title: String) -> bool {
        let clean = title.trim().to_string();
        if clean.is_empty() {
            return false;
        }
        let Some(stored_id) = self.connected_selection() else {
            return false;
        };
        self.mutate(stored_id, SessionMutation::Rename(clean))
    }

    pub fn archive_current(&mut self) -> bool {
        let Some(stored_id) = self.connected_selection() else {
            return false;
        };
        self.mutate(stored_id, SessionMutation::Archive)
    }

    fn connected_selection(&self) -> Option<String> {
        if self.state.connection.as_ref() != Some(&ConnectionState::Connected) {
            return None;
        }
        self.state.selected.clone()
    }

    fn mutate(&mut self, stored_id: String, mutation: SessionMutation) -> bool {
        self.command(Command::MutateSession {
            stored_id,
            mutation,
        })
    }

    pub fn ask_side(&mut self, question: String) -> bool {
        let Some(prepared) = self.prepare_side(&question) else {
            return false;
        };
        self.dispatch_side(prepared, question)
    }

    fn prepare_side(&mut self, question: &str) -> Option<(String, String, u64)> {
        if question.trim().is_empty() {
            return None;
        }
        let stored_id = self.connected_selection()?;
        let runtime_id = self.writable_runtime(&stored_id)?;
        let client_id = self.state.allocate_side_id();
        self.state.inspector_open = true;
        self.state.inspector_tab = InspectorTab::SideAsk;
        self.state
            .conversations
            .get_mut(&stored_id)?
            .begin_side_question(client_id, question.to_string());
        Some((stored_id, runtime_id, client_id))
    }

    fn dispatch_side(&mut self, prepared: (String, String, u64), question: String) -> bool {
        let (stored_id, runtime_id, client_id) = prepared;
        if !self.command(Command::AskSide {
            runtime_id,
            client_id,
            question,
        }) {
            let notice = self.state.notice.clone();
            if let Some(chat) = self.state.conversations.get_mut(&stored_id) {
                chat.fail_side_question(client_id, notice);
            }
            return false;
        }
        self.clear_selected_draft();
        true
    }

    fn clear_selected_draft(&mut self) {
        self.state.set_active_draft(String::new());
        self.bump_draft();
    }

    fn writable_runtime(&self, stored_id: &str) -> Option<String> {
        self.state
            .conversations
            .get(stored_id)
            .filter(|chat| chat.read_only_reason.is_none())
            .and_then(|chat| chat.runtime_id.clone())
    }

    pub fn branch_from(
        &mut self,
        end_inclusive: Option<usize>,
        custom_title: Option<String>,
    ) -> bool {
        let Some((stored_id, command)) = prepare_branch(&self.state, end_inclusive, custom_title)
        else {
            return false;
        };
        self.select_created = true;
        self.state.creating = true;
        self.state.notice.clear();
        self.mark_activity(&stored_id, "Creating branch…");
        if !self.command(command) {
            self.state.creating = false;
            self.select_created = false;
            self.mark_activity(&stored_id, "");
            return false;
        }
        true
    }

    pub fn rewind(&mut self, mode: RewindMode) -> bool {
        let Some((stored_id, command)) = prepare_rewind(&self.state, mode) else {
            return false;
        };
        self.state.notice.clear();
        self.mark_rewinding(&stored_id);
        if !self.command(command) {
            self.clear_rewind(&stored_id);
            return false;
        }
        true
    }

    fn mark_rewinding(&mut self, stored_id: &str) {
        if let Some(chat) = self.state.conversations.get_mut(stored_id) {
            chat.rewinding = true;
            chat.activity = "Rewinding last turn…".into();
        }
    }

    fn clear_rewind(&mut self, stored_id: &str) {
        if let Some(chat) = self.state.conversations.get_mut(stored_id) {
            chat.rewinding = false;
            chat.activity.clear();
        }
    }

    pub fn compress_current(&mut self) -> bool {
        let Some((stored_id, command)) = prepare_compress(&self.state) else {
            return false;
        };
        self.mark_activity(&stored_id, "Compacting context…");
        if !self.command(command) {
            self.mark_activity(&stored_id, "");
            return false;
        }
        if let Some(chat) = self.state.conversations.get_mut(&stored_id) {
            chat.rewinding = true;
        }
        true
    }

    fn mark_activity(&mut self, stored_id: &str, activity: &str) {
        if let Some(chat) = self.state.conversations.get_mut(stored_id) {
            chat.activity = activity.to_string();
        }
    }

    pub fn choose_workspace(&mut self, cwd: Option<String>) -> bool {
        let chosen = blank_to_none(cwd);
        if workspace_starts_new(&self.state, chosen.as_deref()) {
            self.state.launch_cwd = chosen;
            return self.create();
        }
        self.apply_cwd(chosen);
        true
    }

    fn apply_cwd(&mut self, chosen: Option<String>) {
        self.state.launch_cwd = chosen.clone();
        let Some(id) = self.state.selected.clone() else {
            return;
        };
        if let Some(chat) = self.state.conversations.get_mut(&id) {
            chat.telemetry.cwd = chosen.unwrap_or_default();
            chat.inspector_revision = chat.inspector_revision.saturating_add(1);
        }
    }

    pub fn stage_path(&mut self, path: &str) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|_| format!("Could not read {path}"))?;
        let name = file_name(path);
        let id = self.state.allocate_attachment_id();
        let staged = stage_bytes(id, &name, bytes)?;
        self.state.stage_active_images(vec![staged])?;
        Ok(())
    }

    pub fn remove_attachment(&mut self, id: u64) -> bool {
        self.state.remove_active_attachment(id)
    }

    pub fn transcript_export(&self) -> Option<(String, String)> {
        let id = self.state.selected.as_deref()?;
        let title = session_title(&self.state, id);
        let messages = self.selected_messages();
        Some((
            export_file_name(&title),
            transcript_markdown(&title, &messages),
        ))
    }

    pub fn run_palette(&mut self, action: PaletteAction) -> bool {
        if let Some(done) = self.run_navigation(&action) {
            return done;
        }
        self.run_mutation(action)
    }

    fn run_navigation(&mut self, action: &PaletteAction) -> Option<bool> {
        match action {
            PaletteAction::NewChat => Some(self.create()),
            PaletteAction::ToggleSidebar => {
                self.toggle_sidebar();
                Some(true)
            }
            PaletteAction::ToggleInspector => {
                self.state.inspector_open = !self.state.inspector_open;
                Some(true)
            }
            PaletteAction::OpenInspector(tab) => {
                self.state.inspector_open = true;
                self.state.inspector_tab = *tab;
                Some(true)
            }
            PaletteAction::FilterSessions(filter) => {
                self.set_filter(*filter);
                Some(true)
            }
            PaletteAction::FindInTranscript => Some(self.open_find(None)),
            PaletteAction::OpenSession(id) => {
                self.select(id);
                Some(true)
            }
            PaletteAction::OpenSearchHit { stored_id, query } => {
                self.open_hit(stored_id, query);
                Some(true)
            }
            _ => None,
        }
    }

    fn run_mutation(&mut self, action: PaletteAction) -> bool {
        if let Some(done) = self.run_timeline_action(&action) {
            return done;
        }
        self.run_session_action(action)
    }

    fn run_timeline_action(&mut self, action: &PaletteAction) -> Option<bool> {
        match action {
            PaletteAction::BranchCurrent => Some(self.branch_from(None, None)),
            PaletteAction::RewindLast(mode) => Some(self.rewind(*mode)),
            PaletteAction::CompressCurrent => Some(self.compress_current()),
            _ => None,
        }
    }

    fn run_session_action(&mut self, action: PaletteAction) -> bool {
        match action {
            PaletteAction::TogglePinCurrent => self.toggle_pin(),
            PaletteAction::ArchiveCurrent => self.archive_current(),
            PaletteAction::ReloadHistory => {
                self.reload();
                true
            }
            PaletteAction::LoadMoreSessions => {
                self.load_more();
                true
            }
            PaletteAction::Reconnect => {
                self.reconnect();
                true
            }
            PaletteAction::DeleteCurrent => self.delete_current(),
            PaletteAction::FocusFilter | PaletteAction::SwitchModel => true,
            _ => false,
        }
    }

    fn delete_current(&mut self) -> bool {
        let Some(stored_id) = self.connected_selection() else {
            return false;
        };
        self.command(Command::Delete { stored_id })
    }

    fn open_hit(&mut self, stored_id: &str, query: &str) {
        ensure_hit(&mut self.state, stored_id);
        self.select(stored_id);
        let clean = query.trim();
        if clean.is_empty() {
            return;
        }
        if self.needs_resume(stored_id) {
            self.state.pending_find_query = Some(clean.to_string());
            self.state.pending_find_session = Some(stored_id.to_string());
            return;
        }
        let _ = self.open_find(Some(clean));
    }

    fn apply_side(&mut self, event: Event) {
        match event {
            Event::SideStarted {
                runtime_id,
                client_id,
                task_id,
            } => self.with_runtime(&runtime_id, |chat| chat.bind_side_task(client_id, task_id)),
            Event::SideComplete {
                runtime_id,
                task_id,
                question,
                text,
            } => self.with_runtime(&runtime_id, |chat| {
                chat.complete_side_question(&task_id, &question, text);
            }),
            Event::SideFailed {
                runtime_id,
                client_id,
                message,
            } => self.with_runtime(&runtime_id, |chat| {
                chat.fail_side_question(client_id, message)
            }),
            Event::Reasoning {
                runtime_id,
                text,
                replace,
            } => self.with_runtime(&runtime_id, |chat| chat.append_reasoning(&text, replace)),
            Event::ToolTraceEvent { runtime_id, update } => {
                self.with_runtime(&runtime_id, |chat| chat.apply_tool_event(update));
            }
            Event::Telemetry {
                runtime_id,
                telemetry,
            } => self.with_runtime(&runtime_id, |chat| chat.apply_telemetry(telemetry)),
            _ => {}
        }
    }

    fn apply_studio(&mut self, event: Event) {
        match event {
            Event::Workspaces { catalog } => self.state.workspaces = catalog,
            Event::Skills { skills } => self.state.skills = skills,
            Event::Compressed {
                stored_id,
                runtime_id,
                messages,
                before,
                summary,
            } => apply_compressed(
                &mut self.state,
                &stored_id,
                runtime_id,
                messages,
                before,
                &summary,
            ),
            _ => {}
        }
    }

    fn apply_timeline(&mut self, event: Event) {
        match event {
            Event::Branched {
                session,
                parent_title,
                runtime_id,
                messages,
                before,
                model,
            } => self.on_branched(session, parent_title, runtime_id, messages, before, model),
            Event::Undone {
                stored_id,
                runtime_id,
                messages,
                before,
                mode,
                prompt_text,
                ..
            } => self.on_undone(stored_id, runtime_id, messages, before, mode, prompt_text),
            _ => {}
        }
    }

    fn on_branched(
        &mut self,
        session: SessionSummary,
        parent_title: String,
        runtime_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        model: Option<ModelIdentity>,
    ) {
        let id = session.stored_id.clone();
        insert_branch(
            &mut self.state,
            session,
            runtime_id,
            parent_title,
            messages,
            before,
            model,
        );
        self.state.creating = false;
        if self.select_created {
            self.select_created = false;
            self.select(&id);
        }
    }

    fn on_undone(
        &mut self,
        stored_id: String,
        runtime_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        mode: RewindMode,
        prompt_text: String,
    ) {
        note_rewound(
            &mut self.state,
            &stored_id,
            runtime_id.clone(),
            mode,
            &prompt_text,
            messages,
            before,
        );
        if self.state.selected.as_deref() == Some(stored_id.as_str()) {
            self.bump_draft();
        }
        if mode == RewindMode::Regenerate {
            self.send_initial(&stored_id, runtime_id, prompt_text, Vec::new());
        }
    }

    fn on_mutation(&mut self, stored_id: &str, mutation: &SessionMutation) {
        if self.state.apply_mutation(stored_id, mutation) == MutationOutcome::ArchivedSelected {
            self.state.selected = None;
            self.bump_draft();
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

fn is_side_event(event: &Event) -> bool {
    matches!(
        event,
        Event::SideStarted { .. }
            | Event::SideComplete { .. }
            | Event::SideFailed { .. }
            | Event::Reasoning { .. }
            | Event::ToolTraceEvent { .. }
            | Event::Telemetry { .. }
    )
}

fn is_studio_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Workspaces { .. } | Event::Skills { .. } | Event::Compressed { .. }
    )
}

fn is_timeline_event(event: &Event) -> bool {
    matches!(event, Event::Branched { .. } | Event::Undone { .. })
}

fn held_matches(state: &AppState, held: &HeldFiles, session_id: Option<&str>) -> bool {
    let Some(session_id) = session_id else {
        return true;
    };
    session_id == held.stored_id
        || state.stored_for_runtime(session_id).as_deref() == Some(held.stored_id.as_str())
}

fn unique_workspace(state: &AppState, query: Option<&str>) -> Option<Option<String>> {
    let query = query.filter(|text| !text.trim().is_empty())?;
    if state.workspaces.search(query).len() != 1 {
        return None;
    }
    state.workspaces.resolve_query(query)
}

fn outgoing_ready(chat: &Conversation, text: &str) -> bool {
    chat.read_only_reason.is_none()
        && !chat.rewinding
        && chat.status != TurnStatus::Stopping
        && (!text.is_empty() || !chat.attachments.is_empty())
}

fn submit_activity(has_attachments: bool) -> &'static str {
    if has_attachments {
        "Uploading image & sending…"
    } else {
        "Sending…"
    }
}

fn created_conversation(
    runtime_id: String,
    draft: String,
    attachments: Vec<StagedImage>,
    cwd: String,
    model: Option<ModelIdentity>,
) -> Conversation {
    let mut chat = Conversation {
        runtime_id: Some(runtime_id),
        loaded: true,
        draft,
        attachments,
        ..Conversation::default()
    };
    if !cwd.is_empty() {
        chat.telemetry.cwd = cwd;
    }
    if let Some(model) = model {
        chat.confirm_model(model);
    }
    chat
}

fn join_quote(current: &str, quoted: &str) -> String {
    let current = current.trim_end();
    if current.is_empty() {
        format!("{quoted}\n\n")
    } else {
        format!("{current}\n\n{quoted}\n\n")
    }
}

fn session_title(state: &AppState, stored_id: &str) -> String {
    state
        .sessions
        .iter()
        .find(|session| session.stored_id == stored_id)
        .map(|session| session.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or("Conversation")
        .to_string()
}

fn blank_to_none(cwd: Option<String>) -> Option<String> {
    cwd.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn workspace_starts_new(state: &AppState, chosen: Option<&str>) -> bool {
    let Some(id) = &state.selected else {
        return false;
    };
    let current = state
        .conversations
        .get(id)
        .map(|chat| chat.telemetry.cwd.trim())
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(state.workspaces.default_cwd.as_str());
    let target = chosen.unwrap_or(state.workspaces.default_cwd.as_str());
    current != target
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .to_string()
}

fn stage_bytes(id: u64, name: &str, bytes: Vec<u8>) -> Result<StagedImage, String> {
    let owned = bytes.clone();
    StagedImage::from_bytes(id, name, bytes)
        .or_else(|_| StagedImage::from_file_bytes(id, name, owned))
}

fn prepare_branch(
    state: &AppState,
    end_inclusive: Option<usize>,
    custom_title: Option<String>,
) -> Option<(String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) || state.creating {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let chat = state.conversations.get(&stored_id)?;
    if chat.status.busy() || chat.rewinding {
        return None;
    }
    let seed_fallback = seed_messages_through(&chat.messages, end_inclusive);
    if seed_fallback.is_empty() {
        return None;
    }
    Some((
        stored_id.clone(),
        Command::Branch {
            stored_id,
            runtime_id: chat
                .runtime_id
                .clone()
                .filter(|_| chat.read_only_reason.is_none()),
            parent_title: session_title(state, &chat_id(state)),
            before_loaded: chat.history_before,
            visible_count: end_inclusive
                .and_then(|index| visible_branch_count_through(&chat.messages, index)),
            custom_title,
            seed_fallback,
        },
    ))
}

fn chat_id(state: &AppState) -> String {
    state.selected.clone().unwrap_or_default()
}

fn prepare_rewind(state: &AppState, mode: RewindMode) -> Option<(String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let chat = state.conversations.get(&stored_id)?;
    if chat.read_only_reason.is_some() || chat.status.busy() || chat.rewinding {
        return None;
    }
    let runtime_id = chat.runtime_id.clone()?;
    let (_, _, prompt_text) = last_turn_targets(&chat.messages);
    Some((
        stored_id.clone(),
        Command::Undo {
            stored_id,
            runtime_id,
            mode,
            prompt_text: prompt_text?,
        },
    ))
}

fn prepare_compress(state: &AppState) -> Option<(String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let chat = state.conversations.get(&stored_id)?;
    if chat.read_only_reason.is_some()
        || chat.status.busy()
        || chat.rewinding
        || chat.messages.len() < 2
    {
        return None;
    }
    let runtime_id = chat.runtime_id.clone()?;
    Some((
        stored_id.clone(),
        Command::Compress {
            stored_id,
            runtime_id,
        },
    ))
}

fn apply_compressed(
    state: &mut AppState,
    stored_id: &str,
    runtime_id: String,
    messages: Option<Vec<ChatMessage>>,
    before: usize,
    summary: &CompressSummary,
) {
    if let Some(rows) = &messages
        && let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.stored_id == stored_id)
    {
        session.message_count = before + rows.len();
    }
    let Some(chat) = state.conversations.get_mut(stored_id) else {
        return;
    };
    chat.runtime_id = Some(runtime_id);
    chat.rewinding = false;
    chat.activity = summary.status_line();
    if let Some(rows) = messages {
        chat.messages = rows;
        chat.history_before = before;
        chat.revision = chat.revision.saturating_add(1);
    }
    chat.inspector_revision = chat.inspector_revision.saturating_add(1);
}

fn insert_branch(
    state: &mut AppState,
    session: SessionSummary,
    runtime_id: String,
    parent_title: String,
    messages: Vec<ChatMessage>,
    before: usize,
    model: Option<ModelIdentity>,
) {
    let id = session.stored_id.clone();
    let parent_id = session.parent_session_id.clone();
    clear_branch_activity(state, parent_id.as_deref());
    state.upsert_session(session);
    promote_front(state, &id);
    record_parent(state, &id, parent_id.as_deref(), &parent_title);
    state.conversations.insert(
        id,
        branched_conversation(runtime_id, messages, before, &parent_title, model),
    );
}

fn clear_branch_activity(state: &mut AppState, parent_id: Option<&str>) {
    let Some(parent_id) = parent_id else {
        return;
    };
    if let Some(chat) = state.conversations.get_mut(parent_id)
        && chat.activity == "Creating branch…"
    {
        chat.activity.clear();
    }
}

fn promote_front(state: &mut AppState, id: &str) {
    let Some(pos) = state
        .sessions
        .iter()
        .position(|session| session.stored_id == id)
    else {
        return;
    };
    let session = state.sessions.remove(pos);
    state.sessions.insert(0, session);
}

fn record_parent(state: &mut AppState, id: &str, parent_id: Option<&str>, parent_title: &str) {
    if let Some(parent_id) = parent_id {
        state.record_branch(id, parent_id, parent_title);
    } else {
        state.sync_branch_flags();
    }
}

fn branched_conversation(
    runtime_id: String,
    messages: Vec<ChatMessage>,
    before: usize,
    parent_title: &str,
    model: Option<ModelIdentity>,
) -> Conversation {
    let mut chat = Conversation {
        runtime_id: Some(runtime_id),
        messages,
        history_before: before,
        loaded: true,
        activity: format!("Branched from “{parent_title}”."),
        revision: 1,
        inspector_revision: 1,
        ..Default::default()
    };
    if let Some(model) = model {
        chat.confirm_model(model);
    }
    chat
}

fn note_rewound(
    state: &mut AppState,
    stored_id: &str,
    runtime_id: String,
    mode: RewindMode,
    prompt_text: &str,
    messages: Vec<ChatMessage>,
    before: usize,
) {
    if let Some(session) = state
        .sessions
        .iter_mut()
        .find(|session| session.stored_id == stored_id)
    {
        session.message_count = before + messages.len();
    }
    let Some(chat) = state.conversations.get_mut(stored_id) else {
        return;
    };
    chat.runtime_id = Some(runtime_id);
    chat.rewinding = false;
    chat.messages = messages;
    chat.history_before = before;
    chat.status = TurnStatus::Idle;
    chat.interaction = None;
    chat.clear_turn_reasoning();
    chat.revision = chat.revision.saturating_add(1);
    chat.inspector_revision = chat.inspector_revision.saturating_add(1);
    apply_rewind_mode(chat, mode, prompt_text);
}

fn apply_rewind_mode(chat: &mut Conversation, mode: RewindMode, prompt_text: &str) {
    match mode {
        RewindMode::Undo => undo_draft(chat, prompt_text),
        RewindMode::EditRetry => {
            chat.activity = "Turn rewound — edit your prompt and press Enter.".into();
            chat.draft = prompt_text.to_string();
        }
        RewindMode::Regenerate => chat.activity = "Regenerating response…".into(),
    }
}

fn undo_draft(chat: &mut Conversation, prompt_text: &str) {
    chat.activity = "Rewound last turn.".into();
    if chat.draft.trim().is_empty() {
        chat.draft = prompt_text.to_string();
    }
}

fn ensure_hit(state: &mut AppState, stored_id: &str) {
    if state
        .sessions
        .iter()
        .any(|session| session.stored_id == stored_id)
    {
        return;
    }
    let Some(hit) = state
        .remote_search_hits
        .iter()
        .find(|hit| hit.stored_id == stored_id)
        .cloned()
    else {
        return;
    };
    state.upsert_session(hit_summary(&hit));
}

fn hit_summary(hit: &SearchHit) -> SessionSummary {
    SessionSummary {
        stored_id: hit.stored_id.clone(),
        title: hit.title.clone(),
        source: hit.source.clone(),
        updated_at: hit.updated_at.clone(),
        message_count: hit.message_count,
        parent_session_id: hit.parent_session_id.clone(),
        is_branch: hit.parent_session_id.is_some(),
        pinned: hit.pinned,
        preview: hit.snippet_plain.clone(),
        model: hit.model.clone(),
        ..Default::default()
    }
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
                ..Default::default()
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
    fn quote_insert_bumps_the_draft_generation_so_the_caret_lands_at_the_end() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.state
            .conversations
            .get_mut("chat")
            .expect("chat")
            .record_message(ChatMessage {
                id: "m".into(),
                role: Role::Assistant,
                text: "a\n\nb".into(),
            });
        let before = desk.draft_generation;
        assert!(desk.quote_into_draft(0));
        assert!(desk.draft_generation > before);
        assert_eq!(desk.draft(), "> a\n>\n> b\n\n");
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
            stored_id: "chat".into(),
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
        assert_eq!(
            desk.confirm
                .as_ref()
                .map(|prompt| prompt.stored_id.as_str()),
            Some("chat")
        );
    }

    #[test]
    fn observed_usage_does_not_drop_a_pending_confirmation() {
        let mut desk = Desk::offline("");
        desk.apply(created(Some(ModelIdentity {
            provider: "custom:acme".into(),
            model: "one".into(),
        })));
        desk.state.catalog = Catalog::parse(&serde_json::json!({"providers": [
            {"slug": "acme", "name": "Acme", "is_user_defined": true, "models": ["one", "two"]}
        ]}))
        .expect("catalog");
        desk.model_pending = Some(PendingModel {
            stored_id: "chat".into(),
            runtime_id: "rt".into(),
            target: ModelIdentity {
                provider: "custom:acme".into(),
                model: "two".into(),
            },
            prime: None,
        });
        desk.apply(Event::ModelObserved {
            runtime_id: "rt".into(),
            model: "one".into(),
        });
        assert!(desk.model_pending.is_some());
        desk.apply(Event::ModelConfirmation {
            runtime_id: "rt".into(),
            target: ModelIdentity {
                provider: "custom:acme".into(),
                model: "two".into(),
            },
            message: "Confirm.".into(),
        });
        assert_eq!(
            desk.confirm.as_ref().map(|prompt| prompt.message.as_str()),
            Some("Confirm.")
        );
    }

    #[test]
    fn resuming_another_chat_keeps_the_open_confirmation() {
        let mut desk = Desk::offline("");
        desk.apply(created(None));
        desk.confirm = Some(ConfirmPrompt {
            message: "Confirm.".into(),
            stored_id: "chat".into(),
            target: ModelIdentity {
                provider: "custom:acme".into(),
                model: "two".into(),
            },
        });
        desk.apply(Event::Resumed {
            stored_id: "other".into(),
            runtime_id: Some("rt-other".into()),
            messages: Vec::new(),
            before: 0,
            read_only_reason: None,
            model: None,
        });
        assert!(desk.confirm.is_some());
        desk.apply(Event::Resumed {
            stored_id: "chat".into(),
            runtime_id: Some("rt".into()),
            messages: Vec::new(),
            before: 0,
            read_only_reason: None,
            model: None,
        });
        assert!(desk.confirm.is_none());
    }

    fn staged_file() -> StagedImage {
        StagedImage {
            id: 1,
            filename: "shot.png".into(),
            mime_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        }
    }

    #[test]
    fn blank_composer_still_sends_staged_files() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.state
            .conversations
            .get_mut("chat")
            .expect("chat")
            .attachments = vec![staged_file()];
        assert!(!desk.submit());
        assert!(desk.state.notice.contains("Connection configuration"));
        assert_eq!(
            desk.state.conversations["chat"].attachments.len(),
            1,
            "a failed enqueue puts the files back"
        );
    }

    #[test]
    fn submit_error_returns_files_and_acceptance_drops_them() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.held_files = Some(HeldFiles {
            stored_id: "chat".into(),
            files: vec![staged_file()],
        });
        desk.apply(Event::Error {
            operation: "submit".into(),
            session_id: Some("other".into()),
            message: "unrelated".into(),
        });
        assert!(desk.held_files.is_some());
        assert!(desk.state.conversations["chat"].attachments.is_empty());

        desk.apply(Event::Error {
            operation: "submit".into(),
            session_id: Some("rt".into()),
            message: "upload failed".into(),
        });
        assert!(desk.held_files.is_none());
        assert_eq!(desk.state.conversations["chat"].attachments.len(), 1);

        desk.held_files = Some(HeldFiles {
            stored_id: "chat".into(),
            files: vec![staged_file()],
        });
        desk.state
            .conversations
            .get_mut("chat")
            .unwrap()
            .attachments
            .clear();
        desk.apply(Event::Submitted {
            runtime_id: "rt".into(),
        });
        desk.apply(Event::Error {
            operation: "submit".into(),
            session_id: Some("rt".into()),
            message: "too late".into(),
        });
        assert!(desk.held_files.is_none());
        assert!(desk.state.conversations["chat"].attachments.is_empty());
    }

    #[test]
    fn slash_attach_and_workspace_request_a_follow_up() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.set_draft("/attach".into());
        assert!(desk.submit());
        assert_eq!(desk.composer_follow, Some(ComposerFollow::Attach));
        assert!(desk.draft().is_empty());

        desk.set_draft("/workspace native".into());
        assert!(desk.submit());
        assert_eq!(
            desk.composer_follow,
            Some(ComposerFollow::Workspace(Some("native".into())))
        );
    }

    #[test]
    fn switching_chats_rebinds_find_and_keeps_a_parked_query_on_its_session() {
        let mut desk = Desk::offline("");
        desk.select_created = true;
        desk.apply(created(None));
        desk.state.conversations.get_mut("chat").unwrap().messages = vec![ChatMessage {
            id: "c".into(),
            role: Role::User,
            text: "markdown reply".into(),
        }];
        assert!(desk.open_find(Some("markdown")));
        assert_eq!(desk.state.transcript_find.active_message_index(), Some(0));

        desk.state.sessions.push(SessionSummary {
            stored_id: "other".into(),
            title: "Other".into(),
            ..Default::default()
        });
        desk.state.pending_find_query = Some("markdown".into());
        desk.state.pending_find_session = Some("chat".into());
        desk.select("other");
        assert!(desk.state.pending_find_query.is_none());
        assert!(desk.state.transcript_find.matches.is_empty());

        desk.run_palette(PaletteAction::OpenSearchHit {
            stored_id: "late".into(),
            query: "needle".into(),
        });
        assert_eq!(desk.state.pending_find_query.as_deref(), Some("needle"));
        assert_eq!(desk.state.pending_find_session.as_deref(), Some("late"));
        desk.select("other");
        assert!(desk.state.pending_find_query.is_none());

        desk.run_palette(PaletteAction::OpenSearchHit {
            stored_id: "late".into(),
            query: "needle".into(),
        });
        desk.apply(Event::Resumed {
            stored_id: "late".into(),
            runtime_id: Some("rt-late".into()),
            messages: vec![ChatMessage {
                id: "n".into(),
                role: Role::Assistant,
                text: "the needle is here".into(),
            }],
            before: 0,
            read_only_reason: None,
            model: None,
        });
        assert!(desk.state.transcript_find.open);
        assert_eq!(desk.state.transcript_find.query, "needle");
        assert_eq!(desk.state.transcript_find.active_message_index(), Some(0));
        assert!(desk.state.pending_find_query.is_none());
    }
}
