use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;

#[derive(Clone)]
pub struct Config {
    pub url: url::Url,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Config { credentials: [REDACTED] }")
    }
}

impl Config {
    pub fn from_env(trusted_tunnel: bool) -> Result<Self, String> {
        let get = |key| {
            std::env::var(key)
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("Set {key} in the environment or project .env."))
        };
        let raw = get("HERMES_GATEWAY_URL")?;
        let url = url::Url::parse(&raw)
            .map_err(|_| "HERMES_GATEWAY_URL is not a valid URL.".to_string())?;
        Self::validate_url(&url, trusted_tunnel)?;
        Ok(Self {
            url,
            username: get("HERMES_USER")?,
            password: get("HERMES_PASSWORD")?,
        })
    }

    pub fn validate_url(url: &url::Url, trusted_tunnel: bool) -> Result<(), String> {
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "Gateway URL must contain an origin, without credentials, query or fragment."
                    .into(),
            );
        }
        if url.path() != "/" && !url.path().is_empty() {
            return Err("Gateway URL must be the dashboard origin, without a path.".into());
        }
        match url.scheme() {
            "https" => Ok(()),
            "http" if trusted_tunnel && tunnel_host(url.host_str().unwrap_or_default()) => Ok(()),
            "http" => Err("Plain HTTP requires --trusted-tunnel and a loopback or Tailscale address. Use HTTPS otherwise.".into()),
            _ => Err("Gateway URL must use HTTPS (or HTTP inside an explicitly trusted encrypted tunnel).".into()),
        }
    }
}

fn tunnel_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".ts.net") {
        return true;
    }
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_loopback() || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        }
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback()
                || (ip.segments()[0] == 0xfd7a
                    && ip.segments()[1] == 0x115c
                    && ip.segments()[2] == 0xa1e0)
        }
        Err(_) => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected(String),
    AuthenticationFailed(String),
}

#[derive(Clone, Debug, Default)]
pub struct SessionSummary {
    pub stored_id: String,
    pub title: String,
    pub source: String,
    pub updated_at: String,
    pub read_only_reason: Option<String>,
    pub message_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TurnStatus {
    #[default]
    Idle,
    Submitting,
    Streaming,
    AwaitingInput,
    /// Stop acknowledged. Still busy until terminal completion, transport failure, or disconnect.
    Stopping,
    Complete,
    Interrupted,
    Failed,
    Unknown,
}

impl TurnStatus {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Submitting | Self::Streaming | Self::AwaitingInput | Self::Stopping
        )
    }
}

#[derive(Clone, Debug)]
pub enum InteractionKind {
    Approval,
    Clarification,
}

#[derive(Clone, Debug)]
pub struct InteractionRequest {
    pub id: String,
    pub kind: InteractionKind,
    pub prompt: String,
    pub choices: Vec<String>,
    pub questions: Vec<ClarificationQuestion>,
    pub answers: HashMap<String, Option<String>>,
}

#[derive(Clone, Debug)]
pub struct ClarificationQuestion {
    pub qid: String,
    pub prompt: String,
    pub choices: Vec<String>,
    pub multi_select: bool,
}

#[derive(Clone, Debug)]
pub enum InteractionAnswer {
    Approval(String),
    Clarification(HashMap<String, Option<String>>),
    Cancel,
}

#[derive(Clone, Debug)]
pub enum Command {
    List {
        offset: usize,
    },
    Create {
        title: String,
    },
    Resume {
        stored_id: String,
    },
    History {
        stored_id: String,
        before: usize,
    },
    Submit {
        runtime_id: String,
        text: String,
    },
    Interrupt {
        runtime_id: String,
    },
    Respond {
        runtime_id: String,
        request_id: String,
        answer: InteractionAnswer,
    },
    Delete {
        stored_id: String,
    },
    Reconnect,
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum Event {
    Connection(ConnectionState),
    Sessions {
        sessions: Vec<SessionSummary>,
        offset: usize,
        has_more: bool,
    },
    Created {
        session: SessionSummary,
        runtime_id: String,
    },
    Resumed {
        stored_id: String,
        runtime_id: Option<String>,
        messages: Vec<ChatMessage>,
        before: usize,
        read_only_reason: Option<String>,
    },
    History {
        stored_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        next_before: usize,
    },
    Submitted {
        runtime_id: String,
    },
    MessageDelta {
        runtime_id: String,
        message_id: String,
        delta: String,
    },
    MessageComplete {
        runtime_id: String,
        message: ChatMessage,
        status: TurnStatus,
    },
    Activity {
        runtime_id: String,
        text: String,
    },
    Interaction {
        runtime_id: String,
        request: InteractionRequest,
    },
    InteractionCancelled {
        runtime_id: String,
        request_id: String,
    },
    /// Interrupt accepted. Not a terminal completion; the turn stays live until message completion.
    Interrupting {
        runtime_id: String,
    },
    Deleted {
        stored_id: String,
    },
    Error {
        operation: String,
        session_id: Option<String>,
        message: String,
    },
}

#[derive(Default)]
pub struct Conversation {
    pub runtime_id: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub draft: String,
    pub status: TurnStatus,
    pub activity: String,
    pub read_only_reason: Option<String>,
    pub interaction: Option<InteractionRequest>,
    pub loaded: bool,
    pub history_before: usize,
    pub history_loading: bool,
    pub revision: u64,
}

impl Conversation {
    pub fn record_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Display rows include optimistic and streamed messages. They must not
        // move the absolute server-row cursor used by oldest-first paging.
        self.revision += 1;
    }

    pub fn append_delta(&mut self, message_id: String, delta: String) {
        if let Some(message) = self.messages.iter_mut().find(|m| m.id == message_id) {
            message.text.push_str(&delta);
            self.revision += 1;
        } else {
            self.record_message(ChatMessage {
                id: message_id,
                role: Role::Assistant,
                text: delta,
            });
        }
    }

    /// Append streamed text without reopening a stop that has already been acknowledged.
    pub fn receive_delta(&mut self, message_id: String, delta: String) {
        self.append_delta(message_id, delta);
        if self.status == TurnStatus::Stopping {
            return;
        }
        self.status = TurnStatus::Streaming;
        if self.activity != "Receiving response…" {
            self.activity = "Receiving response…".into();
        }
    }

    pub fn finish_message(&mut self, message: ChatMessage) {
        if let Some(old) = self.messages.iter_mut().find(|m| m.id == message.id) {
            if !message.text.is_empty() {
                *old = message;
            }
            self.revision += 1;
        } else if !message.text.is_empty() {
            self.record_message(message);
        }
    }

    pub fn begin_history_page(&mut self) -> Option<usize> {
        if self.history_loading || self.history_before == 0 || self.status.busy() {
            return None;
        }
        self.history_loading = true;
        Some(self.history_before)
    }

    pub fn receive_history_page(
        &mut self,
        before: usize,
        next_before: usize,
        messages: Vec<ChatMessage>,
    ) {
        if !self.history_loading || before != self.history_before {
            return;
        }
        self.history_loading = false;
        self.history_before = next_before;
        self.messages.splice(0..0, messages);
        self.revision += 1;
    }

    pub fn submit_acknowledged(&mut self) {
        if matches!(self.status, TurnStatus::Submitting | TurnStatus::Idle) {
            self.status = TurnStatus::Streaming;
            self.activity = "Hermes is thinking…".into();
        }
    }

    /// Record an interrupt acknowledgement. Draft, history, and runtime identity stay put.
    pub fn begin_stopping(&mut self) {
        self.status = TurnStatus::Stopping;
        self.activity = "Stopping response…".into();
        self.interaction = None;
    }

    /// Adopt a human question. An acknowledged stop stays stopping until the terminal event.
    pub fn receive_interaction(&mut self, request: InteractionRequest) -> bool {
        if self.status == TurnStatus::Stopping {
            return false;
        }
        self.interaction = Some(request);
        self.status = TurnStatus::AwaitingInput;
        self.activity = "Hermes needs your input.".into();
        true
    }

    /// A queued card action may answer only the question that is still open.
    /// Stopping has withdrawn it; do not send or reopen that turn.
    pub fn accepts_response(&self, request_id: &str) -> bool {
        self.status != TurnStatus::Stopping
            && self
                .interaction
                .as_ref()
                .is_some_and(|request| request.id == request_id)
    }

    /// Record a response handed to the backend. Stopping wins over a queued click.
    pub fn response_sent(&mut self) {
        self.interaction = None;
        if self.status == TurnStatus::Stopping {
            return;
        }
        self.status = TurnStatus::Streaming;
        self.activity = "Waiting for Hermes…".into();
    }

    pub fn operation_failed(&mut self, operation: &str, message: String) {
        if self.status.busy() && matches!(operation, "submit" | "gateway") {
            self.status = TurnStatus::Failed;
            self.activity = message;
            self.interaction = None;
        }
    }

    pub fn withdraw_interaction(&mut self, request_id: &str) {
        if self
            .interaction
            .as_ref()
            .is_some_and(|r| r.id == request_id)
        {
            self.interaction = None;
            if self.status == TurnStatus::AwaitingInput {
                self.status = TurnStatus::Streaming;
                self.activity = "The question was resolved. Hermes is continuing…".into();
            }
        }
    }
}

#[derive(Default)]
pub struct AppState {
    pub sessions: Vec<SessionSummary>,
    pub conversations: HashMap<String, Conversation>,
    pub selected: Option<String>,
    pub connection: Option<ConnectionState>,
    pub list_has_more: bool,
    pub list_loading: bool,
    pub creating: bool,
    pub notice: String,
}

impl AppState {
    pub fn conversation_for_runtime_mut(&mut self, runtime: &str) -> Option<&mut Conversation> {
        self.conversations
            .values_mut()
            .find(|c| c.runtime_id.as_deref() == Some(runtime))
    }

    pub fn stored_for_runtime(&self, runtime: &str) -> Option<String> {
        self.conversations
            .iter()
            .find_map(|(id, c)| (c.runtime_id.as_deref() == Some(runtime)).then(|| id.clone()))
    }

    pub fn upsert_session(&mut self, session: SessionSummary) {
        if let Some(old) = self
            .sessions
            .iter_mut()
            .find(|s| s.stored_id == session.stored_id)
        {
            *old = session;
        } else {
            self.sessions.push(session);
        }
    }

    pub fn disconnect(&mut self) {
        for conversation in self.conversations.values_mut() {
            if conversation.status.busy() {
                conversation.status = TurnStatus::Unknown;
                conversation.activity = "Connection lost. This turn may have reached the server; it will not be sent again. Reconnect to reload server history.".into();
                conversation.interaction = None;
                conversation.revision += 1;
            }
            conversation.runtime_id = None;
            conversation.history_loading = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_plaintext_is_rejected_even_with_trust_flag() {
        assert!(
            Config::validate_url(&url::Url::parse("http://example.com").unwrap(), true).is_err()
        );
        assert!(
            Config::validate_url(&url::Url::parse("https://example.com").unwrap(), false).is_ok()
        );
        assert!(
            Config::validate_url(&url::Url::parse("http://100.64.0.2:9119").unwrap(), false)
                .is_err()
        );
        assert!(
            Config::validate_url(&url::Url::parse("http://100.64.0.2:9119").unwrap(), true).is_ok()
        );
    }
    #[test]
    fn disconnect_never_replays_or_completes_uncertain_turns() {
        let mut state = AppState::default();
        state.conversations.insert(
            "durable".into(),
            Conversation {
                runtime_id: Some("ephemeral".into()),
                status: TurnStatus::Submitting,
                draft: "next turn".into(),
                ..Default::default()
            },
        );
        state.disconnect();
        let c = &state.conversations["durable"];
        assert_eq!(c.status, TurnStatus::Unknown);
        assert_eq!(c.runtime_id, None);
        assert_eq!(c.draft, "next turn");
        assert!(state.conversations.contains_key("durable"));
    }
    #[test]
    fn final_response_replaces_streamed_text_without_duplicate_rows() {
        let mut conversation = Conversation::default();
        conversation.append_delta("reply".into(), "Part".into());
        conversation.append_delta("reply".into(), "ial".into());
        conversation.finish_message(ChatMessage {
            id: "reply".into(),
            role: Role::Assistant,
            text: "Final".into(),
        });
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.messages[0].text, "Final");
        conversation.finish_message(ChatMessage {
            id: "reply".into(),
            role: Role::Assistant,
            text: String::new(),
        });
        assert_eq!(conversation.messages[0].text, "Final");
    }

    #[test]
    fn optimistic_rows_do_not_shift_server_history_cursor() {
        let mut conversation = Conversation {
            history_before: 50,
            ..Default::default()
        };
        conversation.record_message(ChatMessage {
            id: "client-1".into(),
            role: Role::User,
            text: "rejected prompt".into(),
        });
        assert_eq!(conversation.begin_history_page(), Some(50));
        assert_eq!(conversation.begin_history_page(), None);
        conversation.receive_history_page(
            50,
            0,
            vec![ChatMessage {
                id: "saved".into(),
                role: Role::User,
                text: "earlier".into(),
            }],
        );
        conversation.receive_history_page(
            50,
            0,
            vec![ChatMessage {
                id: "duplicate".into(),
                role: Role::User,
                text: "earlier".into(),
            }],
        );
        assert_eq!(
            conversation
                .messages
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["saved", "client-1"]
        );
        assert_eq!(conversation.history_before, 0);
    }

    #[test]
    fn late_ack_cannot_reopen_finished_or_waiting_turns() {
        let mut conversation = Conversation {
            status: TurnStatus::Complete,
            ..Default::default()
        };
        conversation.submit_acknowledged();
        assert_eq!(conversation.status, TurnStatus::Complete);
        conversation.status = TurnStatus::AwaitingInput;
        conversation.submit_acknowledged();
        assert_eq!(conversation.status, TurnStatus::AwaitingInput);
        conversation.status = TurnStatus::Submitting;
        conversation.submit_acknowledged();
        assert_eq!(conversation.status, TurnStatus::Streaming);
        conversation.status = TurnStatus::Stopping;
        conversation.activity = "Stopping response…".into();
        conversation.submit_acknowledged();
        assert_eq!(conversation.status, TurnStatus::Stopping);
        assert_eq!(conversation.activity, "Stopping response…");
        assert!(conversation.status.busy());
    }

    #[test]
    fn withdrawing_a_question_keeps_the_turn_running() {
        let mut conversation = Conversation {
            status: TurnStatus::AwaitingInput,
            interaction: Some(InteractionRequest {
                id: "request".into(),
                kind: InteractionKind::Clarification,
                prompt: "Question".into(),
                choices: Vec::new(),
                questions: Vec::new(),
                answers: HashMap::new(),
            }),
            ..Default::default()
        };
        conversation.withdraw_interaction("different");
        assert!(conversation.interaction.is_some());
        conversation.withdraw_interaction("request");
        assert!(conversation.interaction.is_none());
        assert_eq!(conversation.status, TurnStatus::Streaming);
    }

    #[test]
    fn only_submit_and_gateway_failures_end_a_busy_turn() {
        let pending = InteractionRequest {
            id: "approval".into(),
            kind: InteractionKind::Approval,
            prompt: "Allow?".into(),
            choices: vec!["yes".into()],
            questions: Vec::new(),
            answers: HashMap::new(),
        };
        let mut conversation = Conversation {
            status: TurnStatus::AwaitingInput,
            activity: "Waiting for approval".into(),
            interaction: Some(pending),
            ..Default::default()
        };
        conversation.operation_failed("interrupt", "timed out".into());
        assert_eq!(conversation.status, TurnStatus::AwaitingInput);
        assert_eq!(conversation.activity, "Waiting for approval");
        assert!(conversation.interaction.is_some());
        conversation.status = TurnStatus::Streaming;
        conversation.operation_failed("interrupt", "rejected".into());
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert_eq!(conversation.activity, "Waiting for approval");
        assert!(conversation.interaction.is_some());

        conversation.status = TurnStatus::Streaming;
        conversation.operation_failed("gateway", "transport closed".into());
        assert_eq!(conversation.status, TurnStatus::Failed);
        assert_eq!(conversation.activity, "transport closed");
        assert!(conversation.interaction.is_none());

        conversation.status = TurnStatus::Submitting;
        conversation.activity = "Sending…".into();
        conversation.operation_failed("submit", "rejected".into());
        assert_eq!(conversation.status, TurnStatus::Failed);
        assert_eq!(conversation.activity, "rejected");

        conversation.status = TurnStatus::Stopping;
        conversation.activity = "Stopping response…".into();
        conversation.interaction = Some(InteractionRequest {
            id: "approval".into(),
            kind: InteractionKind::Approval,
            prompt: "Allow?".into(),
            choices: vec!["yes".into()],
            questions: Vec::new(),
            answers: HashMap::new(),
        });
        conversation.operation_failed("interrupt", "timed out".into());
        assert_eq!(conversation.status, TurnStatus::Stopping);
        assert!(conversation.status.busy());
        assert_eq!(conversation.activity, "Stopping response…");
        assert!(conversation.interaction.is_some());
        conversation.operation_failed("gateway", "transport closed".into());
        assert_eq!(conversation.status, TurnStatus::Failed);
        assert_eq!(conversation.activity, "transport closed");
        assert!(conversation.interaction.is_none());
    }

    #[test]
    fn stopping_stays_busy_through_late_ack_and_delta_until_disconnect() {
        let mut live = Conversation::default();
        live.receive_delta("reply".into(), "Hi".into());
        assert_eq!(live.status, TurnStatus::Streaming);
        assert_eq!(live.activity, "Receiving response…");
        assert_eq!(live.messages[0].text, "Hi");

        let mut state = AppState::default();
        state.conversations.insert(
            "durable".into(),
            Conversation {
                runtime_id: Some("ephemeral".into()),
                draft: "next turn".into(),
                history_before: 12,
                messages: vec![ChatMessage {
                    id: "reply".into(),
                    role: Role::Assistant,
                    text: "Part".into(),
                }],
                interaction: Some(InteractionRequest {
                    id: "approval".into(),
                    kind: InteractionKind::Approval,
                    prompt: "Allow?".into(),
                    choices: vec!["once".into()],
                    questions: Vec::new(),
                    answers: HashMap::new(),
                }),
                ..Default::default()
            },
        );
        {
            let conversation = state
                .conversation_for_runtime_mut("ephemeral")
                .expect("runtime");
            assert!(conversation.interaction.is_some());
            conversation.begin_stopping();
            assert!(conversation.status.busy());
            assert_eq!(conversation.status, TurnStatus::Stopping);
            assert_eq!(conversation.activity, "Stopping response…");
            assert!(conversation.interaction.is_none());
            assert_eq!(conversation.draft, "next turn");
            assert_eq!(conversation.runtime_id.as_deref(), Some("ephemeral"));
            assert_eq!(conversation.history_before, 12);
            assert_eq!(conversation.messages[0].text, "Part");

            conversation.submit_acknowledged();
            assert_eq!(conversation.status, TurnStatus::Stopping);
            assert_eq!(conversation.activity, "Stopping response…");
            assert!(conversation.status.busy());

            conversation.receive_delta("reply".into(), "ial".into());
            assert_eq!(conversation.messages[0].text, "Partial");
            assert_eq!(conversation.status, TurnStatus::Stopping);
            assert_eq!(conversation.activity, "Stopping response…");
            assert!(conversation.status.busy());
            assert_eq!(conversation.draft, "next turn");
            assert_eq!(conversation.runtime_id.as_deref(), Some("ephemeral"));
            assert_eq!(conversation.history_before, 12);
        }
        state.disconnect();
        let conversation = &state.conversations["durable"];
        assert_eq!(conversation.status, TurnStatus::Unknown);
        assert!(!conversation.status.busy());
        assert_eq!(
            conversation.activity,
            "Connection lost. This turn may have reached the server; it will not be sent again. Reconnect to reload server history."
        );
        assert_eq!(conversation.draft, "next turn");
        assert_eq!(conversation.runtime_id, None);
        assert_eq!(conversation.messages[0].text, "Partial");
        assert_eq!(conversation.history_before, 12);
        assert!(conversation.interaction.is_none());
    }

    #[test]
    fn stopping_ignores_late_interaction_and_queued_answer() {
        let request = InteractionRequest {
            id: "approval".into(),
            kind: InteractionKind::Approval,
            prompt: "Allow?".into(),
            choices: vec!["once".into()],
            questions: Vec::new(),
            answers: HashMap::new(),
        };
        let mut conversation = Conversation {
            runtime_id: Some("ephemeral".into()),
            draft: "next turn".into(),
            history_before: 12,
            status: TurnStatus::Stopping,
            activity: "Stopping response…".into(),
            messages: vec![ChatMessage {
                id: "reply".into(),
                role: Role::Assistant,
                text: "Partial".into(),
            }],
            interaction: Some(request.clone()),
            ..Default::default()
        };
        assert!(!conversation.receive_interaction(request.clone()));
        assert!(!conversation.accepts_response("approval"));
        conversation.response_sent();
        assert_eq!(conversation.status, TurnStatus::Stopping);
        assert!(conversation.status.busy());
        assert_eq!(conversation.activity, "Stopping response…");
        assert!(conversation.interaction.is_none());
        assert_eq!(conversation.draft, "next turn");
        assert_eq!(conversation.runtime_id.as_deref(), Some("ephemeral"));
        assert_eq!(conversation.history_before, 12);
        assert_eq!(conversation.messages[0].text, "Partial");
        assert_eq!(conversation.revision, 0);

        conversation.status = TurnStatus::Streaming;
        conversation.activity = "Receiving response…".into();
        assert!(conversation.receive_interaction(request.clone()));
        assert_eq!(conversation.status, TurnStatus::AwaitingInput);
        assert_eq!(conversation.activity, "Hermes needs your input.");
        assert!(conversation.accepts_response("approval"));
        assert!(!conversation.accepts_response("other"));
        conversation.response_sent();
        assert!(conversation.interaction.is_none());
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert_eq!(conversation.activity, "Waiting for Hermes…");
        assert_eq!(conversation.draft, "next turn");
        assert_eq!(conversation.runtime_id.as_deref(), Some("ephemeral"));
        assert_eq!(conversation.history_before, 12);
        assert_eq!(conversation.messages[0].text, "Partial");
    }
}
