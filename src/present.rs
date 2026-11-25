//! Labels and enablement derived from [`crate::model::AppState`].
//!
//! The GPUI view paints these values. It does not decide them.

use std::collections::{HashMap, HashSet};

use crate::model::{
    AppState, Catalog, ConnectionState, Conversation, InteractionRequest, ModelIdentity,
    ModelState, Role, TurnStatus,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Live,
    Error,
}

pub struct ConnectionView {
    pub label: &'static str,
    pub detail: String,
    pub tone: Tone,
}

pub struct Controls {
    pub send: bool,
    pub stop_visible: bool,
    pub stop_enabled: bool,
    pub editable: bool,
    pub new_chat: bool,
    pub more_visible: bool,
    pub more_enabled: bool,
    pub reconnect: bool,
    pub older_visible: bool,
    pub older_enabled: bool,
    pub latest_visible: bool,
    pub latest_enabled: bool,
    pub model_enabled: bool,
    pub activity: String,
    pub spinning: bool,
    pub title: String,
    pub readonly: Option<String>,
    pub show_messages: bool,
}

#[derive(Clone, Debug)]
pub struct QuestionDraft {
    pub qid: String,
    pub prompt: String,
    pub choices: Vec<String>,
    pub multi: bool,
    pub locked: Option<Option<String>>,
    pub skipped: bool,
    pub text: String,
    pub selected: Vec<bool>,
}

pub fn connection_view(state: Option<&ConnectionState>, configured: bool) -> ConnectionView {
    match state {
        Some(ConnectionState::Connected) => live("Connected"),
        Some(ConnectionState::Connecting) => neutral("Connecting…"),
        Some(ConnectionState::AuthenticationFailed(message)) => {
            error("Authentication failed", message)
        }
        Some(ConnectionState::Disconnected(message)) => error("Disconnected", message),
        None if configured => neutral("Connecting…"),
        None => neutral("Not configured"),
    }
}

fn live(label: &'static str) -> ConnectionView {
    ConnectionView {
        label,
        detail: String::new(),
        tone: Tone::Live,
    }
}

fn neutral(label: &'static str) -> ConnectionView {
    ConnectionView {
        label,
        detail: String::new(),
        tone: Tone::Neutral,
    }
}

fn error(label: &'static str, detail: &str) -> ConnectionView {
    ConnectionView {
        label,
        detail: detail.to_string(),
        tone: Tone::Error,
    }
}

pub fn controls(state: &AppState, pending: &HashSet<String>, configured: bool) -> Controls {
    let conversation = open_conversation(state);
    let connected = state.connection.as_ref() == Some(&ConnectionState::Connected);
    let reload = reloading(state, pending);
    let write = writable(connected, conversation);
    Controls {
        send: send_enabled(write, reload, conversation)
            || (state.launch_ready() && launch_has_input(state)),
        stop_visible: conversation.is_some_and(|chat| chat.status.busy()),
        stop_enabled: stop_enabled(connected, conversation),
        editable: chat_editable(conversation) || state.launch_ready(),
        new_chat: connected && !state.creating,
        more_visible: state.list_has_more,
        more_enabled: connected && !state.list_loading,
        reconnect: configured && !connecting(state),
        older_visible: conversation.is_some_and(|chat| chat.history_before > 0),
        older_enabled: older_enabled(conversation),
        latest_visible: state.selected.is_some(),
        latest_enabled: connected && !reload,
        model_enabled: state.model_picker_enabled(conversation),
        activity: activity_text(conversation, reload),
        spinning: spinning(conversation, reload),
        title: window_title(state),
        readonly: conversation.and_then(|chat| chat.read_only_reason.clone()),
        show_messages: conversation.is_some_and(|chat| !chat.messages.is_empty()),
    }
}

fn open_conversation(state: &AppState) -> Option<&Conversation> {
    state
        .selected
        .as_ref()
        .and_then(|id| state.conversations.get(id))
}

fn reloading(state: &AppState, pending: &HashSet<String>) -> bool {
    state
        .selected
        .as_ref()
        .is_some_and(|id| pending.contains(id))
}

fn writable(connected: bool, conversation: Option<&Conversation>) -> bool {
    connected && conversation.is_some_and(ready_to_write)
}

fn ready_to_write(conversation: &Conversation) -> bool {
    conversation.runtime_id.is_some() && conversation.read_only_reason.is_none()
}

fn send_enabled(write: bool, reload: bool, conversation: Option<&Conversation>) -> bool {
    write && !reload && conversation.is_some_and(can_send)
}

fn can_send(conversation: &Conversation) -> bool {
    conversation.status != TurnStatus::Stopping
        && !conversation.rewinding
        && (!conversation.draft.trim().is_empty() || !conversation.attachments.is_empty())
}

fn chat_editable(conversation: Option<&Conversation>) -> bool {
    conversation.is_some_and(|chat| chat.read_only_reason.is_none() && !chat.rewinding)
}

fn launch_has_input(state: &AppState) -> bool {
    !state.launch_draft.trim().is_empty() || !state.launch_attachments.is_empty()
}

/// Markdown block quote, capped so a huge reply stays editable.
pub fn quote_markdown(text: &str) -> String {
    const MAX_LINES: usize = 12;
    let lines: Vec<&str> = text.trim().lines().collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(MAX_LINES)
        .map(|line| quote_line(line))
        .collect();
    if lines.len() > MAX_LINES {
        out.push("> …".to_string());
    }
    out.join("\n")
}

fn quote_line(line: &str) -> String {
    if line.is_empty() {
        ">".to_string()
    } else {
        format!("> {line}")
    }
}

fn stop_enabled(connected: bool, conversation: Option<&Conversation>) -> bool {
    connected && !conversation.is_some_and(|chat| chat.status == TurnStatus::Stopping)
}

fn connecting(state: &AppState) -> bool {
    matches!(state.connection, Some(ConnectionState::Connecting))
}

fn older_enabled(conversation: Option<&Conversation>) -> bool {
    conversation.is_some_and(|chat| !chat.status.busy() && !chat.history_loading)
}

fn spinning(conversation: Option<&Conversation>, reload: bool) -> bool {
    reload || conversation.is_some_and(|chat| chat.status.busy() || chat.history_loading)
}

fn activity_text(conversation: Option<&Conversation>, reload: bool) -> String {
    if reload {
        return "Loading server history…".into();
    }
    conversation
        .map(|chat| chat.activity.clone())
        .unwrap_or_default()
}

fn window_title(state: &AppState) -> String {
    state
        .selected
        .as_ref()
        .and_then(|id| {
            state
                .sessions
                .iter()
                .find(|session| &session.stored_id == id)
        })
        .map(session_title)
        .unwrap_or_else(|| "Hermes Native".into())
}

fn session_title(session: &crate::model::SessionSummary) -> String {
    if session.title.is_empty() {
        "Untitled conversation".into()
    } else {
        session.title.clone()
    }
}

pub fn completion_activity(status: &TurnStatus) -> String {
    match status {
        TurnStatus::Complete => String::new(),
        TurnStatus::Interrupted => "Response stopped. Partial text may be incomplete.".into(),
        TurnStatus::Failed => "The server reported a failed response.".into(),
        TurnStatus::Idle
        | TurnStatus::Submitting
        | TurnStatus::Streaming
        | TurnStatus::AwaitingInput
        | TurnStatus::Stopping
        | TurnStatus::Unknown => "Response ended without a confirmed completion.".into(),
    }
}

pub fn model_caption(model: &ModelState) -> String {
    match model {
        ModelState::Unknown => "Choose a model".into(),
        ModelState::Confirmed(identity) => identity.model.clone(),
        ModelState::Switching { target, .. } => format!("Switching to {}…", target.model),
        ModelState::Queued { target, .. } => format!("{} · next turn", target.model),
        ModelState::Unresolved(Some(identity)) => format!("{} · unconfirmed", identity.model),
        ModelState::Unresolved(None) => "Model unconfirmed".into(),
    }
}

pub fn provider_name(identity: &ModelIdentity, catalog: &Catalog) -> String {
    catalog
        .options
        .iter()
        .find(|option| &option.identity == identity)
        .map_or_else(
            || identity.provider.clone(),
            |option| option.provider_name.clone(),
        )
}

pub fn model_provider_line(model: &ModelState, catalog: &Catalog) -> String {
    match model {
        ModelState::Unknown => "server default".into(),
        ModelState::Confirmed(identity) | ModelState::Unresolved(Some(identity)) => {
            provider_name(identity, catalog)
        }
        ModelState::Switching { target, .. } | ModelState::Queued { target, .. } => {
            provider_name(target, catalog)
        }
        ModelState::Unresolved(None) => "unconfirmed".into(),
    }
}

pub fn model_tooltip(model: &ModelState, catalog: &Catalog) -> String {
    match model {
        ModelState::Unknown => "Choose the model for this conversation".into(),
        ModelState::Confirmed(identity) => confirmed_tip(identity, catalog),
        ModelState::Switching { target, .. } => format!("Switching to {} …", target.model),
        ModelState::Queued { target, .. } => queued_tip(target, catalog),
        ModelState::Unresolved(Some(identity)) => unresolved_tip(identity, catalog),
        ModelState::Unresolved(None) => {
            "The last change was never confirmed. Pick again to be sure.".into()
        }
    }
}

fn confirmed_tip(identity: &ModelIdentity, catalog: &Catalog) -> String {
    format!(
        "{} · {}\nChanging this affects only this conversation.",
        identity.model,
        provider_name(identity, catalog)
    )
}

fn queued_tip(identity: &ModelIdentity, catalog: &Catalog) -> String {
    format!(
        "{} · {}\nApplies from the next message; the running turn keeps its model.",
        identity.model,
        provider_name(identity, catalog)
    )
}

fn unresolved_tip(identity: &ModelIdentity, catalog: &Catalog) -> String {
    format!(
        "{} · {}\nThe last change was never confirmed. Pick again to be sure.",
        identity.model,
        provider_name(identity, catalog)
    )
}

pub fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "YOU",
        Role::Assistant => "HERMES",
        Role::Tool => "TOOL",
        Role::System => "SYSTEM",
    }
}

pub fn approval_label(choice: &str) -> &str {
    match choice {
        "once" => "Allow once",
        "session" => "Allow for this chat",
        "always" => "Always allow",
        "deny" => "Deny",
        other => other,
    }
}

pub fn approval_choices(request: &InteractionRequest) -> Vec<String> {
    if request.choices.is_empty() {
        vec!["once".into(), "deny".into()]
    } else {
        request.choices.clone()
    }
}

pub fn picker_status(catalog: &Catalog, matches: usize) -> String {
    if let Some(error) = &catalog.error {
        return error.clone();
    }
    if catalog.loading {
        return "Loading models…".into();
    }
    if catalog.is_empty() {
        return "No models are configured on this server.".into();
    }
    if matches == 0 {
        return "No model matches that search.".into();
    }
    String::new()
}

pub fn session_heading(title: &str) -> String {
    if title.is_empty() {
        "Untitled conversation".into()
    } else {
        title.to_string()
    }
}

pub fn session_meta(source: &str, read_only: bool) -> String {
    if read_only {
        format!("{source} · read-only")
    } else {
        source.to_string()
    }
}

pub fn collect_answers(
    questions: &[QuestionDraft],
) -> Result<HashMap<String, Option<String>>, &'static str> {
    let mut answers = HashMap::new();
    for question in questions {
        if answers.contains_key(&question.qid) {
            return Err("duplicate");
        }
        answers.insert(question.qid.clone(), one_answer(question)?);
    }
    Ok(answers)
}

fn one_answer(question: &QuestionDraft) -> Result<Option<String>, &'static str> {
    if let Some(locked) = &question.locked {
        return Ok(locked.clone());
    }
    if question.skipped {
        return Ok(None);
    }
    if question.choices.is_empty() {
        return text_answer(&question.text);
    }
    choice_answer(question)
}

fn text_answer(text: &str) -> Result<Option<String>, &'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Err("empty")
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn choice_answer(question: &QuestionDraft) -> Result<Option<String>, &'static str> {
    let selected = selected_choices(question);
    if selected.is_empty() || (!question.multi && selected.len() != 1) {
        return Err("choice");
    }
    if question.multi {
        return serde_json::to_string(&selected)
            .map(Some)
            .map_err(|_| "encode");
    }
    Ok(selected.first().cloned())
}

fn selected_choices(question: &QuestionDraft) -> Vec<String> {
    question
        .choices
        .iter()
        .enumerate()
        .filter(|(index, _)| question.selected.get(*index).copied().unwrap_or(false))
        .map(|(_, choice)| choice.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(model: &str) -> ModelIdentity {
        ModelIdentity {
            provider: "custom:acme".into(),
            model: model.into(),
        }
    }

    fn catalog() -> Catalog {
        Catalog::parse(&serde_json::json!({"providers": [
            {"slug": "acme", "name": "Acme", "is_user_defined": true, "models": ["one", "two"]}
        ]}))
        .expect("catalog")
    }

    #[test]
    fn trigger_text_never_claims_an_unconfirmed_selection() {
        let catalog = catalog();
        assert_eq!(model_caption(&ModelState::Unknown), "Choose a model");
        assert_eq!(
            model_provider_line(&ModelState::Unknown, &catalog),
            "server default"
        );
        assert_eq!(model_caption(&ModelState::Confirmed(pair("one"))), "one");
        assert_eq!(
            model_provider_line(&ModelState::Confirmed(pair("one")), &catalog),
            "Acme"
        );
        assert_eq!(
            model_caption(&ModelState::Switching {
                from: Some(pair("one")),
                target: pair("two")
            }),
            "Switching to two…"
        );
        assert_eq!(
            model_caption(&ModelState::Queued {
                from: Some(pair("one")),
                target: pair("two")
            }),
            "two · next turn"
        );
        assert_eq!(
            model_caption(&ModelState::Unresolved(None)),
            "Model unconfirmed"
        );
        assert_eq!(
            model_caption(&ModelState::Unresolved(Some(pair("one")))),
            "one · unconfirmed"
        );
    }

    #[test]
    fn queued_switch_says_it_applies_from_the_next_message() {
        let catalog = catalog();
        let tooltip = model_tooltip(
            &ModelState::Queued {
                from: Some(pair("one")),
                target: pair("two"),
            },
            &catalog,
        );
        assert!(tooltip.contains("next message"), "{tooltip}");
        assert!(tooltip.contains("Acme"), "{tooltip}");
    }

    #[test]
    fn send_requires_a_draft_on_a_writable_chat() {
        let pending = HashSet::new();
        assert!(!controls(&writable_state(Conversation::default()), &pending, true).send);
        assert!(
            controls(
                &writable_state(ready_chat(TurnStatus::Idle)),
                &pending,
                true
            )
            .send
        );
        assert!(
            !controls(
                &writable_state(ready_chat(TurnStatus::Stopping)),
                &pending,
                true
            )
            .send
        );
    }

    fn writable_state(chat: Conversation) -> AppState {
        let mut conversations = HashMap::new();
        conversations.insert("chat".into(), chat);
        AppState {
            connection: Some(ConnectionState::Connected),
            selected: Some("chat".into()),
            conversations,
            ..AppState::default()
        }
    }

    fn ready_chat(status: TurnStatus) -> Conversation {
        Conversation {
            runtime_id: Some("rt".into()),
            draft: "  hello".into(),
            status,
            ..Conversation::default()
        }
    }

    #[test]
    fn empty_approval_choices_fall_back_to_once_and_deny() {
        let request = InteractionRequest {
            id: "1".into(),
            kind: crate::model::InteractionKind::Approval,
            prompt: String::new(),
            choices: Vec::new(),
            questions: Vec::new(),
            answers: HashMap::new(),
        };
        assert_eq!(approval_choices(&request), vec!["once", "deny"]);
        assert_eq!(approval_label("once"), "Allow once");
    }

    #[test]
    fn skipped_and_locked_answers_round_trip() {
        let questions = vec![
            QuestionDraft {
                qid: "a".into(),
                prompt: "A".into(),
                choices: Vec::new(),
                multi: false,
                locked: Some(Some("kept".into())),
                skipped: false,
                text: String::new(),
                selected: Vec::new(),
            },
            QuestionDraft {
                qid: "b".into(),
                prompt: "B".into(),
                choices: Vec::new(),
                multi: false,
                locked: None,
                skipped: true,
                text: String::new(),
                selected: Vec::new(),
            },
        ];
        let answers = collect_answers(&questions).expect("answers");
        assert_eq!(answers.get("a").map(Option::as_deref), Some(Some("kept")));
        assert_eq!(answers.get("b").map(Option::as_deref), Some(None));
    }

    #[test]
    fn quote_markdown_prefixes_and_caps_lines() {
        assert_eq!(quote_markdown("a\n\nb"), "> a\n>\n> b");
        let long = (0..20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let quoted = quote_markdown(&long);
        assert!(quoted.ends_with("> …"));
        assert_eq!(quoted.lines().count(), 13);
    }
}
