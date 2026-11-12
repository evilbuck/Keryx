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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSummary {
    pub stored_id: String,
    pub title: String,
    pub source: String,
    pub updated_at: String,
    pub read_only_reason: Option<String>,
    pub message_count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionFilter {
    #[default]
    All,
    Chats,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Interactive,
    Automated,
}

pub fn classify_source(source: &str) -> SourceKind {
    let trimmed = source.trim();
    let automated = [
        "cron",
        "heartbeat",
        "webhook",
        "schedule",
        "scheduler",
        "kanban",
        "subagent",
        "background",
    ]
    .iter()
    .any(|candidate| trimmed.eq_ignore_ascii_case(candidate));
    if automated {
        SourceKind::Automated
    } else {
        SourceKind::Interactive
    }
}

impl SessionFilter {
    pub fn matches(&self, session: &SessionSummary) -> bool {
        match self {
            Self::All => true,
            Self::Chats => classify_source(&session.source) == SourceKind::Interactive,
            Self::Auto => classify_source(&session.source) == SourceKind::Automated,
        }
    }
}

pub fn source_badge(source: &str) -> String {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return "CHAT".into();
    }
    if trimmed.eq_ignore_ascii_case("api_server") {
        return "API".into();
    }
    trimmed.chars().take(10).collect::<String>().to_uppercase()
}

pub fn format_relative_age(updated_at: &str, now_epoch_secs: i64) -> Option<String> {
    let raw = updated_at
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite() && *f > 0.0)
        .map(|f| f as i64)?;
    let epoch = if raw > 1_000_000_000_000 {
        raw / 1_000
    } else {
        raw
    };
    let delta = (now_epoch_secs - epoch).max(0);
    Some(relative_bucket(delta))
}

fn relative_bucket(delta: i64) -> String {
    if delta < 60 {
        "just now".into()
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else if delta < 604_800 {
        format!("{}d ago", delta / 86_400)
    } else {
        format!("{}w ago", delta / 604_800)
    }
}

pub fn session_meta_line(session: &SessionSummary, now_epoch_secs: i64) -> String {
    let mut parts = vec![message_count_label(session.message_count)];
    if let Some(age) = format_relative_age(&session.updated_at, now_epoch_secs) {
        parts.push(age);
    }
    if session.read_only_reason.is_some() {
        parts.push("read-only".into());
    }
    parts.join(" · ")
}

fn message_count_label(count: usize) -> String {
    match count {
        0 => "empty".into(),
        1 => "1 msg".into(),
        many => format!("{many} msgs"),
    }
}

pub fn derive_launch_title(prompt: &str) -> String {
    let first = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let cleaned = first.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "New conversation".into();
    }
    if collapsed.chars().count() <= 48 {
        return collapsed;
    }
    let prefix: String = collapsed.chars().take(47).collect();
    format!("{}…", prefix.trim_end())
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

    /// Busy enough to steer, but not yet Stopping. Stopping stays closed to steers.
    pub fn steerable(&self) -> bool {
        matches!(
            self,
            Self::Submitting | Self::Streaming | Self::AwaitingInput
        )
    }
}

/// What the gateway did with a steer. Neither outcome ends the live turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerOutcome {
    /// Folded into the live turn; the same stream continues.
    Redirected,
    /// Queued for after the current response.
    Queued,
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

/// What a `config.set` model reply means. An acknowledgement is not, by itself,
/// proof that the next inference will use the pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchReply {
    Rejected(String),
    NeedsConfirmation(String),
    Accepted {
        resolved: ModelIdentity,
        deferred: bool,
    },
}

pub fn interpret_switch(target: &ModelIdentity, result: &serde_json::Value) -> SwitchReply {
    let scope = result
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if scope != "session" {
        return SwitchReply::Rejected(
            "The gateway did not confirm a conversation-scoped change.".into(),
        );
    }
    if result
        .get("confirm_required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let message = result
            .get("confirm_message")
            .and_then(serde_json::Value::as_str)
            .filter(|message| !message.is_empty())
            .unwrap_or("This model needs an explicit confirmation.");
        return SwitchReply::NeedsConfirmation(message.to_string());
    }
    let resolved = result
        .get("value")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| ModelIdentity {
            provider: target.provider.clone(),
            model: value.to_string(),
        })
        .unwrap_or_else(|| target.clone());
    let deferred = result
        .get("deferred")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    SwitchReply::Accepted { resolved, deferred }
}

/// A provider/model pair. The provider is the gateway token the switch value must
/// carry; custom providers are canonicalized to `custom:<slug>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelIdentity {
    pub provider: String,
    pub model: String,
}

impl ModelIdentity {
    pub fn parse(provider: Option<&str>, model: Option<&str>) -> Option<Self> {
        let provider = provider.map(str::trim).filter(|s| !s.is_empty())?;
        let model = model.map(str::trim).filter(|s| !s.is_empty())?;
        Some(Self {
            provider: provider.to_string(),
            model: model.to_string(),
        })
    }

    /// The gateway tokenizes the switch value with `split()`, so a pair containing
    /// whitespace cannot be represented and must never be offered.
    pub fn representable(&self) -> bool {
        let safe = |value: &str| {
            !value.is_empty() && !value.starts_with('-') && !value.chars().any(char::is_whitespace)
        };
        safe(&self.provider) && safe(&self.model)
    }

    /// `--session` keeps the change on this conversation. Omitting it can persist the
    /// choice as the profile default.
    pub fn switch_value(&self) -> String {
        format!("{} --provider {} --session", self.model, self.provider)
    }

    /// The prime must name no provider: with an explicit provider the gateway stops
    /// waiting for the agent build, which is the path that acknowledges a switch
    /// without applying it to the agent it is still constructing.
    pub fn prime_value(&self) -> String {
        format!("{} --session", self.model)
    }
}

/// One selectable catalog row. Same-named models under different providers stay separate.
#[derive(Clone, Debug)]
pub struct ModelOption {
    pub identity: ModelIdentity,
    pub provider_name: String,
    search_key: String,
}

impl ModelOption {
    pub fn selectable(&self) -> bool {
        self.identity.representable()
    }
}

/// The configured catalog for the active connection.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub options: Vec<ModelOption>,
    pub default: Option<ModelIdentity>,
    pub loading: bool,
    pub error: Option<String>,
}

impl Catalog {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let providers = value
            .get("providers")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "The model catalog was not usable.".to_string())?;
        let default = ModelIdentity::parse(
            value.get("provider").and_then(serde_json::Value::as_str),
            value.get("model").and_then(serde_json::Value::as_str),
        );
        let mut options = Vec::new();
        for row in providers {
            let Some(slug) = row
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .filter(|slug| !slug.is_empty())
            else {
                continue;
            };
            let name = row
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(slug);
            let custom = row
                .get("is_user_defined")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let provider = if custom && !slug.starts_with("custom:") {
                format!("custom:{slug}")
            } else {
                slug.to_string()
            };
            let models = row
                .get("models")
                .and_then(serde_json::Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter(|model| !model.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for model in models {
                options.push(ModelOption {
                    identity: ModelIdentity {
                        provider: provider.clone(),
                        model: model.to_string(),
                    },
                    provider_name: name.to_string(),
                    search_key: search_key(name, slug, model),
                });
            }
        }
        Ok(Self {
            options,
            default,
            loading: false,
            error: None,
        })
    }

    /// Indices in display order. An empty query keeps catalog order; otherwise every
    /// whitespace-separated token must match, ranked by contiguity then position.
    pub fn search(&self, query: &str) -> Vec<usize> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return (0..self.options.len()).collect();
        }
        let mut ranked: Vec<(usize, usize)> = self
            .options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                match_tokens(&option.search_key, &tokens).map(|score| (score, index))
            })
            .collect();
        ranked.sort_by_key(|&(score, index)| (score, index));
        ranked.into_iter().map(|(_, index)| index).collect()
    }

    /// First catalog pair serving `model`, used to reconcile an observed inference model.
    pub fn identity_for_model(&self, model: &str) -> Option<ModelIdentity> {
        self.options
            .iter()
            .find(|option| option.identity.model == model)
            .map(|option| option.identity.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.options.is_empty()
    }
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(normalize)
        .filter(|token| !token.is_empty())
        .collect()
}

fn match_tokens(haystack: &str, tokens: &[String]) -> Option<usize> {
    let mut total = 0usize;
    for token in tokens {
        total += token_score(haystack, token)?;
    }
    Some(total)
}

fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

fn search_key(name: &str, slug: &str, model: &str) -> String {
    normalize(&format!("{name} {slug} {model}"))
}

/// Lower is better. Contiguous occurrences beat gapped subsequences; a word start is
/// preferred over a mid-word hit.
fn token_score(haystack: &str, needle: &str) -> Option<usize> {
    if let Some(index) = haystack.find(needle) {
        let word_start = index == 0 || haystack.as_bytes()[index - 1] == b' ';
        return Some(if word_start { 0 } else { 1 });
    }
    let mut chars = needle.chars();
    let mut next = chars.next()?;
    let mut gaps = 0usize;
    for character in haystack.chars() {
        if character == next {
            match chars.next() {
                Some(following) => next = following,
                None => return Some(10 + gaps),
            }
        } else {
            gaps += 1;
        }
    }
    None
}

/// What the composer shows for one conversation's model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModelState {
    /// No confirmed identity yet, such as a chat created before the catalog loaded.
    #[default]
    Unknown,
    Confirmed(ModelIdentity),
    /// A switch is in flight; the outcome is not known yet.
    Switching {
        from: Option<ModelIdentity>,
        target: ModelIdentity,
    },
    /// The gateway deferred the switch to the next turn.
    Queued {
        from: Option<ModelIdentity>,
        target: ModelIdentity,
    },
    /// A transport outcome that never resolved. Never silently retried.
    Unresolved(Option<ModelIdentity>),
}

impl ModelState {
    pub fn confirmed(&self) -> Option<&ModelIdentity> {
        match self {
            Self::Confirmed(identity) => Some(identity),
            Self::Unknown | Self::Switching { .. } | Self::Queued { .. } | Self::Unresolved(_) => {
                None
            }
        }
    }

    /// True while a switch outcome is unknown; another switch must be refused.
    pub fn switching(&self) -> bool {
        matches!(self, Self::Switching { .. })
    }

    pub fn target(&self) -> Option<&ModelIdentity> {
        match self {
            Self::Switching { target, .. } | Self::Queued { target, .. } => Some(target),
            _ => None,
        }
    }
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
    /// Busy-turn steer. The backend routes it to `session.redirect`.
    Steer {
        runtime_id: String,
        text: String,
    },
    Respond {
        runtime_id: String,
        request_id: String,
        answer: InteractionAnswer,
    },
    Delete {
        stored_id: String,
    },
    /// Fetch the configured catalog for this connection.
    FetchModels,
    /// Switch this conversation's model. `prime` is the conversation's own identity,
    /// sent first on a chat whose agent is not built yet; the gateway only honors the
    /// scoped switch after that build exists.
    SetModel {
        runtime_id: String,
        prime: Option<ModelIdentity>,
        target: ModelIdentity,
        confirm_expensive: bool,
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
        model: Option<ModelIdentity>,
    },
    Resumed {
        stored_id: String,
        runtime_id: Option<String>,
        messages: Vec<ChatMessage>,
        before: usize,
        read_only_reason: Option<String>,
        model: Option<ModelIdentity>,
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
    /// The gateway accepted a busy-turn steer. The live turn continues untouched.
    SteerAccepted {
        runtime_id: String,
        outcome: SteerOutcome,
    },
    /// The gateway started a server-initiated turn (a FIFO-drained queued
    /// prompt, possibly after a reconnect). No client submit is involved.
    TurnRolled {
        runtime_id: String,
    },
    Deleted {
        stored_id: String,
    },
    /// Live title update broadcast by the gateway for a conversation.
    SessionTitle {
        session_id: String,
        title: String,
    },
    /// The configured catalog, or the failure that prevented loading it.
    Catalog {
        catalog: Catalog,
    },
    /// The gateway accepted a switch. `deferred` means it applies from the next turn.
    ModelAck {
        runtime_id: String,
        resolved: ModelIdentity,
        deferred: bool,
    },
    /// The gateway rejected the switch outright; the previous selection stands.
    ModelRejected {
        runtime_id: String,
        message: String,
    },
    /// The gateway requires an explicit confirmation for this pair.
    ModelConfirmation {
        runtime_id: String,
        target: ModelIdentity,
        message: String,
    },
    /// The outcome never arrived. The conversation stays unconfirmed.
    ModelAmbiguous {
        runtime_id: String,
    },
    /// The model an actual inference used, read from the completion's usage.
    ModelObserved {
        runtime_id: String,
        model: String,
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
    pub model: ModelState,
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

    /// Record a steer outcome. The live turn keeps its status and generation:
    /// a steer never reopens Stopping and never claims Submitting.
    pub fn steer_acknowledged(&mut self, outcome: SteerOutcome) {
        if !self.status.steerable() {
            return;
        }
        match outcome {
            SteerOutcome::Redirected => {
                if self.status == TurnStatus::Submitting {
                    self.status = TurnStatus::Streaming;
                }
                self.activity = "Steer received. Hermes is adjusting…".into();
            }
            SteerOutcome::Queued => {
                self.activity = "Queued. Waiting for this response to finish…".into();
            }
        }
        self.revision += 1;
    }

    /// The gateway rolled the settled turn into a server-initiated follow-up.
    /// Restores Streaming without claiming a client submit; Stopping and a
    /// pending submit are never overridden.
    pub fn turn_rolled(&mut self) {
        if self.status.busy() {
            return;
        }
        self.status = TurnStatus::Streaming;
        self.activity = "Hermes is starting the queued follow-up…".into();
        self.revision += 1;
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

    /// Create and cold-resume metadata are the only authoritative stored identity.
    pub fn confirm_model(&mut self, identity: ModelIdentity) {
        self.model = ModelState::Confirmed(identity);
        self.revision += 1;
    }

    /// Start a switch. Refused while an earlier outcome is still unknown.
    pub fn begin_model_switch(&mut self, target: ModelIdentity) -> bool {
        if self.model.switching() {
            return false;
        }
        let from = match &self.model {
            ModelState::Confirmed(identity) => Some(identity.clone()),
            ModelState::Queued { target, .. } => Some(target.clone()),
            ModelState::Unresolved(last) => last.clone(),
            ModelState::Unknown | ModelState::Switching { .. } => None,
        };
        self.model = ModelState::Switching { from, target };
        self.revision += 1;
        true
    }

    /// `confirm` is for a chat that has already run. A primed empty chat's
    /// acknowledgement is not proof: a later inference has been observed to ignore it.
    pub fn model_acknowledged(&mut self, resolved: ModelIdentity, deferred: bool, confirm: bool) {
        let ModelState::Switching { from, target: _ } = self.model.clone() else {
            return;
        };
        self.model = if deferred {
            ModelState::Queued {
                from,
                target: resolved,
            }
        } else if confirm {
            ModelState::Confirmed(resolved)
        } else {
            ModelState::Switching {
                from,
                target: resolved,
            }
        };
        self.revision += 1;
    }

    /// A definite rejection keeps the previous selection.
    pub fn model_rejected(&mut self) {
        if let ModelState::Switching { from, .. } = self.model.clone() {
            self.model = from.map_or(ModelState::Unknown, ModelState::Confirmed);
            self.revision += 1;
        }
    }

    /// No answer arrived. Do not pretend either outcome.
    pub fn model_ambiguous(&mut self) {
        if let ModelState::Switching { from, .. } = self.model.clone() {
            self.model = ModelState::Unresolved(from);
            self.revision += 1;
        }
    }

    /// Reconcile with the model an inference actually used.
    pub fn observe_model(&mut self, identity: ModelIdentity) {
        let next = match &self.model {
            ModelState::Switching { .. } => ModelState::Confirmed(identity),
            ModelState::Confirmed(current) if *current == identity => return,
            ModelState::Confirmed(_) => ModelState::Confirmed(identity),
            ModelState::Unknown => ModelState::Confirmed(identity),
            ModelState::Queued { target, .. } if *target == identity => {
                ModelState::Confirmed(identity)
            }
            ModelState::Queued { .. } => return,
            ModelState::Unresolved(last) if last.as_ref() == Some(&identity) => {
                ModelState::Unresolved(last.clone())
            }
            ModelState::Unresolved(_) => ModelState::Confirmed(identity),
        };
        self.model = next;
        self.revision += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewChat,
    SwitchModel,
    ToggleSidebar,
    FocusFilter,
    FilterSessions(SessionFilter),
    ReloadHistory,
    LoadMoreSessions,
    Reconnect,
    DeleteCurrent,
    OpenSession(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    pub action: PaletteAction,
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    pub shortcut: &'static str,
    pub badge: String,
    pub active: bool,
    pub destructive: bool,
}

impl PaletteEntry {
    fn command(
        action: PaletteAction,
        icon: &'static str,
        title: impl Into<String>,
        subtitle: impl Into<String>,
        shortcut: &'static str,
    ) -> Self {
        Self {
            action,
            icon,
            title: title.into(),
            subtitle: subtitle.into(),
            shortcut,
            badge: String::new(),
            active: false,
            destructive: false,
        }
    }

    fn search_key(&self) -> String {
        normalize(&format!("{} {} {}", self.title, self.subtitle, self.badge))
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
    pub catalog: Catalog,
    pub session_filter: SessionFilter,
    pub session_query: String,
    pub launch_draft: String,
    pub pending_launch: Option<String>,
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

    pub fn filter_counts(&self) -> (usize, usize, usize) {
        let chats = self
            .sessions
            .iter()
            .filter(|s| classify_source(&s.source) == SourceKind::Interactive)
            .count();
        let all = self.sessions.len();
        (all, chats, all.saturating_sub(chats))
    }

    /// Indices into `self.sessions` matching the active filter and sidebar search query.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let tokens = query_tokens(&self.session_query);
        let filtered = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| self.session_filter.matches(s));
        if tokens.is_empty() {
            return filtered.map(|(index, _)| index).collect();
        }
        let mut ranked: Vec<(usize, usize)> = filtered
            .filter_map(|(index, s)| {
                let key = normalize(&format!("{} {} {}", s.title, s.source, s.stored_id));
                match_tokens(&key, &tokens).map(|score| (score, index))
            })
            .collect();
        ranked.sort_by_key(|&(score, index)| (score, index));
        ranked.into_iter().map(|(_, index)| index).collect()
    }

    pub fn active_draft(&self) -> &str {
        match &self.selected {
            Some(id) => self.conversations.get(id).map_or("", |c| c.draft.as_str()),
            None => &self.launch_draft,
        }
    }

    pub fn set_active_draft(&mut self, text: String) {
        if let Some(id) = self.selected.clone() {
            self.conversations.entry(id).or_default().draft = text;
        } else {
            self.launch_draft = text;
        }
    }

    /// Begin creating a conversation from the launch composer (`selected == None`).
    /// Keeps `launch_draft` intact until `Created` succeeds so a failure never loses text.
    pub fn begin_launch_submit(&mut self) -> Option<(String, String)> {
        if self.selected.is_some() || self.creating || self.pending_launch.is_some() {
            return None;
        }
        let trimmed = self.launch_draft.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        let title = derive_launch_title(&trimmed);
        self.creating = true;
        self.pending_launch = Some(trimmed.clone());
        Some((title, trimmed))
    }

    pub fn cancel_pending_launch(&mut self) {
        self.pending_launch = None;
        self.creating = false;
    }

    pub fn take_pending_launch(&mut self) -> Option<String> {
        let prompt = self.pending_launch.take()?;
        self.launch_draft.clear();
        Some(prompt)
    }

    pub fn take_launch_draft_for_blank_chat(&mut self) -> String {
        if self.pending_launch.is_some() {
            return String::new();
        }
        std::mem::take(&mut self.launch_draft)
    }

    pub fn apply_session_title(&mut self, id: &str, title: &str) -> bool {
        let clean = title.trim();
        if clean.is_empty() {
            return false;
        }
        let target = if self.sessions.iter().any(|s| s.stored_id == id) {
            Some(id.to_string())
        } else {
            self.stored_for_runtime(id)
        };
        let Some(stored_id) = target else {
            return false;
        };
        let Some(session) = self.sessions.iter_mut().find(|s| s.stored_id == stored_id) else {
            return false;
        };
        if session.title == clean {
            return false;
        }
        session.title = clean.to_string();
        true
    }

    /// Remove a deleted session and report whether it was the open conversation.
    pub fn remove_session(&mut self, stored_id: &str) -> bool {
        self.sessions.retain(|s| s.stored_id != stored_id);
        self.conversations.remove(stored_id);
        if self.selected.as_deref() == Some(stored_id) {
            self.selected = None;
            true
        } else {
            false
        }
    }

    pub fn disconnect(&mut self) {
        self.cancel_pending_launch();
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

pub fn build_palette_entries(
    state: &AppState,
    query: &str,
    now_epoch_secs: i64,
) -> Vec<PaletteEntry> {
    let mut all = palette_commands(state);
    let cmd_count = all.len();
    all.extend(palette_sessions(state, now_epoch_secs));
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return all;
    }
    let mut ranked: Vec<(usize, u8, usize, PaletteEntry)> = all
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let score = match_tokens(&entry.search_key(), &tokens)?;
            let kind_rank = u8::from(index >= cmd_count);
            Some((score, kind_rank, index, entry))
        })
        .collect();
    ranked.sort_by_key(|&(score, kind_rank, index, _)| (score, kind_rank, index));
    ranked.into_iter().map(|(_, _, _, entry)| entry).collect()
}

fn palette_commands(state: &AppState) -> Vec<PaletteEntry> {
    let connected = state.connection.as_ref() == Some(&ConnectionState::Connected);
    let conversation = state
        .selected
        .as_ref()
        .and_then(|id| state.conversations.get(id));
    let writable = connected
        && conversation.is_some_and(|c| c.runtime_id.is_some() && c.read_only_reason.is_none());
    let mut out = Vec::new();
    if connected && !state.creating {
        out.push(PaletteEntry::command(
            PaletteAction::NewChat,
            "list-add-symbolic",
            "New conversation",
            "Create a fresh server-owned chat",
            "Ctrl+N",
        ));
    }
    if writable && conversation.is_some_and(|c| !c.model.switching()) {
        out.push(PaletteEntry::command(
            PaletteAction::SwitchModel,
            "preferences-system-symbolic",
            "Switch model…",
            "Choose provider and model for this conversation",
            "Ctrl+M",
        ));
    }
    out.extend(palette_filter_commands(state));
    out.extend(palette_utility_commands(state, connected));
    out
}

fn palette_filter_commands(state: &AppState) -> Vec<PaletteEntry> {
    let (all, chats, auto) = state.filter_counts();
    let specs = [
        (
            SessionFilter::Chats,
            "user-available-symbolic",
            "Show interactive chats only",
            format!("{chats} human conversations · hides cron"),
        ),
        (
            SessionFilter::Auto,
            "alarm-symbolic",
            "Show automations & cron",
            format!("{auto} scheduled or background sessions"),
        ),
        (
            SessionFilter::All,
            "view-list-symbolic",
            "Show all conversations",
            format!("{all} loaded conversations"),
        ),
    ];
    specs
        .into_iter()
        .map(|(filter, icon, title, subtitle)| {
            let mut entry = PaletteEntry::command(
                PaletteAction::FilterSessions(filter),
                icon,
                title,
                subtitle,
                "",
            );
            entry.active = state.session_filter == filter;
            if entry.active {
                entry.badge = "ACTIVE".into();
            }
            entry
        })
        .collect()
}

fn palette_utility_commands(state: &AppState, connected: bool) -> Vec<PaletteEntry> {
    let mut out = vec![
        PaletteEntry::command(
            PaletteAction::ToggleSidebar,
            "sidebar-show-symbolic",
            "Toggle conversations sidebar",
            "Collapse or expand the left session rail",
            "Ctrl+B",
        ),
        PaletteEntry::command(
            PaletteAction::FocusFilter,
            "system-search-symbolic",
            "Filter sidebar conversations",
            "Focus the sidebar search input",
            "Ctrl+F",
        ),
    ];
    if connected && state.selected.is_some() {
        out.push(PaletteEntry::command(
            PaletteAction::ReloadHistory,
            "view-refresh-symbolic",
            "Reload conversation history",
            "Re-fetch saved transcript from the server",
            "Ctrl+R",
        ));
    }
    if connected && state.list_has_more && !state.list_loading {
        out.push(PaletteEntry::command(
            PaletteAction::LoadMoreSessions,
            "content-loading-symbolic",
            "Load more conversations",
            "Fetch the next page of 50 sessions from the server",
            "",
        ));
    }
    if !matches!(state.connection, Some(ConnectionState::Connecting)) {
        out.push(PaletteEntry::command(
            PaletteAction::Reconnect,
            "network-transmit-receive-symbolic",
            "Reconnect to Hermes gateway",
            "Re-open the WebSocket and refresh session metadata",
            "",
        ));
    }
    if connected && state.selected.is_some() {
        let mut del = PaletteEntry::command(
            PaletteAction::DeleteCurrent,
            "user-trash-symbolic",
            "Delete current conversation…",
            "Permanently close and remove this session on the server",
            "",
        );
        del.destructive = true;
        out.push(del);
    }
    out
}

fn palette_sessions(state: &AppState, now_epoch_secs: i64) -> Vec<PaletteEntry> {
    state
        .sessions
        .iter()
        .map(|session| {
            let icon = if classify_source(&session.source) == SourceKind::Automated {
                "alarm-symbolic"
            } else {
                "mail-send-symbolic"
            };
            let title = if session.title.is_empty() {
                "Untitled conversation".into()
            } else {
                session.title.clone()
            };
            PaletteEntry {
                action: PaletteAction::OpenSession(session.stored_id.clone()),
                icon,
                title,
                subtitle: session_meta_line(session, now_epoch_secs),
                shortcut: "",
                badge: source_badge(&session.source),
                active: state.selected.as_deref() == Some(session.stored_id.as_str()),
                destructive: false,
            }
        })
        .collect()
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
    fn steers_are_allowed_exactly_when_not_stopping() {
        use TurnStatus::*;
        assert!(Streaming.steerable());
        assert!(Submitting.steerable());
        assert!(AwaitingInput.steerable());
        assert!(!Stopping.steerable());
        assert!(!Idle.steerable());
        assert!(!Complete.steerable());
        assert!(!Unknown.steerable());
    }

    #[test]
    fn steer_acknowledged_keeps_turn_state_and_never_reopens_stopping() {
        let mut conversation = Conversation {
            status: TurnStatus::Streaming,
            ..Default::default()
        };
        conversation.steer_acknowledged(SteerOutcome::Redirected);
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert!(conversation.activity.contains("Steer"));

        conversation.status = TurnStatus::AwaitingInput;
        conversation.steer_acknowledged(SteerOutcome::Redirected);
        assert_eq!(conversation.status, TurnStatus::AwaitingInput);

        conversation.status = TurnStatus::Submitting;
        conversation.steer_acknowledged(SteerOutcome::Redirected);
        assert_eq!(conversation.status, TurnStatus::Streaming);

        conversation.status = TurnStatus::Stopping;
        conversation.activity = "Stopping response…".into();
        conversation.steer_acknowledged(SteerOutcome::Redirected);
        assert_eq!(conversation.status, TurnStatus::Stopping);

        conversation.steer_acknowledged(SteerOutcome::Queued);
        assert_eq!(conversation.status, TurnStatus::Stopping);
        assert!(conversation.activity.contains("Stopping"));

        conversation.status = TurnStatus::Streaming;
        conversation.steer_acknowledged(SteerOutcome::Queued);
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert!(conversation.activity.contains("Queued"));
    }

    #[test]
    fn steer_optimistic_rows_do_not_shift_server_history_cursor() {
        let mut conversation = Conversation {
            history_before: 50,
            ..Default::default()
        };
        conversation.status = TurnStatus::Streaming;
        conversation.record_message(ChatMessage {
            id: "client-steer".into(),
            role: Role::User,
            text: "steer text".into(),
        });
        // The optimistic row is display-only: the server-row cursor stays put.
        assert_eq!(conversation.history_before, 50);
        assert_eq!(
            conversation.messages.last().map(|m| m.text.as_str()),
            Some("steer text")
        );
    }

    #[test]
    fn turn_rolled_restores_streaming_only_from_settled_states() {
        let mut conversation = Conversation {
            status: TurnStatus::Complete,
            ..Default::default()
        };
        conversation.turn_rolled();
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert!(conversation.activity.contains("follow-up"));

        conversation.status = TurnStatus::Stopping;
        conversation.activity = "Stopping response…".into();
        conversation.turn_rolled();
        assert_eq!(conversation.status, TurnStatus::Stopping);
        assert!(conversation.activity.contains("Stopping"));

        conversation.status = TurnStatus::Submitting;
        conversation.turn_rolled();
        assert_eq!(conversation.status, TurnStatus::Submitting);

        conversation.status = TurnStatus::Streaming;
        conversation.activity = "Receiving response…".into();
        conversation.turn_rolled();
        assert_eq!(conversation.status, TurnStatus::Streaming);
        assert_eq!(conversation.activity, "Receiving response…");
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
    fn catalog_keeps_provider_distinctions_and_rejects_unrepresentable_tokens() {
        let catalog = Catalog::parse(&serde_json::json!({
            "model": "shared", "provider": "custom:acme",
            "providers": [
                {"slug": "acme", "name": "Acme", "is_user_defined": true, "models": ["shared", "not safe"]},
                {"slug": "other", "name": "Other", "models": ["shared"]}
            ]
        }))
        .expect("catalog");
        assert_eq!(catalog.options.len(), 3);
        assert_eq!(catalog.options[0].identity.provider, "custom:acme");
        assert_eq!(catalog.options[2].identity.provider, "other");
        assert_ne!(catalog.options[0].identity, catalog.options[2].identity);
        assert!(catalog.options[0].selectable());
        assert!(!catalog.options[1].selectable());
        assert_eq!(
            catalog.options[0].identity.switch_value(),
            "shared --provider custom:acme --session"
        );
        assert_eq!(
            catalog.options[0].identity.prime_value(),
            "shared --session"
        );
        assert_eq!(catalog.default, Some(catalog.options[0].identity.clone()));
    }

    #[test]
    fn fuzzy_search_ranks_contiguous_matches_and_keeps_catalog_order_on_ties() {
        let catalog = Catalog::parse(&serde_json::json!({"providers": [
            {"slug": "cloud", "name": "Cloud", "models": ["g-l-m", "glm", "glm"]},
            {"slug": "other", "name": "Other", "models": ["glm"]}
        ]}))
        .expect("catalog");
        assert_eq!(catalog.search(""), vec![0, 1, 2, 3]);
        assert_eq!(catalog.search("cloud glm"), vec![1, 2, 0]);
        assert_eq!(catalog.search("other glm"), vec![3]);
        assert!(catalog.search("missing").is_empty());
    }

    #[test]
    fn switch_replies_keep_scope_confirmation_and_deferral_distinct() {
        let target = ModelIdentity {
            provider: "custom:acme".into(),
            model: "next".into(),
        };
        assert_eq!(
            interpret_switch(&target, &serde_json::json!({"scope": "global"})),
            SwitchReply::Rejected(
                "The gateway did not confirm a conversation-scoped change.".into()
            )
        );
        assert_eq!(
            interpret_switch(
                &target,
                &serde_json::json!({"scope": "session", "confirm_required": true, "confirm_message": "expensive"})
            ),
            SwitchReply::NeedsConfirmation("expensive".into())
        );
        assert_eq!(
            interpret_switch(
                &target,
                &serde_json::json!({"scope": "session", "value": "next", "deferred": true})
            ),
            SwitchReply::Accepted {
                resolved: target.clone(),
                deferred: true
            }
        );
        assert!(matches!(
            interpret_switch(&target, &serde_json::json!({"scope": "session"})),
            SwitchReply::Accepted {
                deferred: false,
                ..
            }
        ));
    }

    #[test]
    fn a_primed_acknowledgement_waits_for_the_inference() {
        let original = ModelIdentity {
            provider: "p".into(),
            model: "a".into(),
        };
        let target = ModelIdentity {
            provider: "p".into(),
            model: "b".into(),
        };
        let mut conversation = Conversation {
            model: ModelState::Confirmed(original.clone()),
            ..Default::default()
        };
        conversation.begin_model_switch(target.clone());
        conversation.model_acknowledged(target.clone(), false, false);
        assert!(conversation.model.switching());
        conversation.observe_model(target.clone());
        assert_eq!(conversation.model.confirmed(), Some(&target));

        conversation.begin_model_switch(original.clone());
        conversation.model_acknowledged(original.clone(), false, false);
        let other = ModelIdentity {
            provider: "p".into(),
            model: "other".into(),
        };
        conversation.observe_model(other.clone());
        assert_eq!(conversation.model.confirmed(), Some(&other));
    }

    #[test]
    fn a_switch_stays_unconfirmed_until_evidence_arrives() {
        let target = ModelIdentity {
            provider: "custom:acme".into(),
            model: "next".into(),
        };
        let original = ModelIdentity {
            provider: "custom:acme".into(),
            model: "first".into(),
        };
        let mut conversation = Conversation::default();
        conversation.confirm_model(original.clone());
        assert!(conversation.begin_model_switch(target.clone()));
        assert!(!conversation.begin_model_switch(target.clone()));
        assert_eq!(conversation.model.confirmed(), None);

        // A deferred reply is the gateway's own next-turn statement.
        conversation.model_acknowledged(target.clone(), true, false);
        assert_eq!(
            conversation.model,
            ModelState::Queued {
                from: Some(original.clone()),
                target: target.clone()
            }
        );
        // The inference that used it confirms the pair.
        conversation.observe_model(target.clone());
        assert_eq!(conversation.model.confirmed(), Some(&target));
    }

    #[test]
    fn rejection_restores_previous_selection_and_ambiguity_stays_unresolved() {
        let original = ModelIdentity {
            provider: "p".into(),
            model: "a".into(),
        };
        let target = ModelIdentity {
            provider: "p".into(),
            model: "b".into(),
        };
        let mut conversation = Conversation {
            model: ModelState::Confirmed(original.clone()),
            ..Default::default()
        };
        conversation.begin_model_switch(target.clone());
        conversation.model_rejected();
        assert_eq!(conversation.model.confirmed(), Some(&original));

        conversation.begin_model_switch(target);
        conversation.model_ambiguous();
        assert_eq!(
            conversation.model,
            ModelState::Unresolved(Some(original.clone()))
        );
        assert!(!conversation.model.switching());
        // An unconfirmed earlier attempt never becomes a silent claim.
        assert_eq!(conversation.model.confirmed(), None);
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

    fn sample_session(id: &str, title: &str, source: &str, count: usize) -> SessionSummary {
        SessionSummary {
            stored_id: id.into(),
            title: title.into(),
            source: source.into(),
            updated_at: "1700000000".into(),
            read_only_reason: None,
            message_count: count,
        }
    }

    #[test]
    fn triage_separates_interactive_chats_from_automations_and_fuzzy_ranks() {
        let mut state = AppState {
            sessions: vec![
                sample_session("s1", "Install hermes-push plugin", "tui", 8),
                sample_session("s2", "stock-daily-self-review", "cron", 2),
                sample_session("s3", "Set up Cloudflare Tunnel", "local", 14),
                sample_session("s4", "board-worker", "kanban", 4),
            ],
            ..Default::default()
        };
        assert_eq!(state.filter_counts(), (4, 2, 2));
        assert_eq!(state.visible_sessions(), vec![0, 1, 2, 3]);
        state.session_filter = SessionFilter::Chats;
        assert_eq!(state.visible_sessions(), vec![0, 2]);
        state.session_filter = SessionFilter::Auto;
        assert_eq!(state.visible_sessions(), vec![1, 3]);
        state.session_filter = SessionFilter::All;
        state.session_query = "cloudflare".into();
        assert_eq!(state.visible_sessions(), vec![2]);
    }

    #[test]
    fn session_metadata_formats_badges_counts_and_relative_ages() {
        assert_eq!(source_badge(""), "CHAT");
        assert_eq!(source_badge("api_server"), "API");
        assert_eq!(source_badge("cron"), "CRON");
        assert_eq!(format_relative_age("0", 1_700_000_100), None);
        assert_eq!(
            format_relative_age("1700000090", 1_700_000_100).as_deref(),
            Some("just now")
        );
        assert_eq!(
            format_relative_age("1700000000000", 1_700_000_300).as_deref(),
            Some("5m ago")
        );
        assert_eq!(
            format_relative_age("1699992800", 1_700_000_000).as_deref(),
            Some("2h ago")
        );
        assert_eq!(
            format_relative_age("1699827200", 1_700_000_000).as_deref(),
            Some("2d ago")
        );
        assert_eq!(
            format_relative_age("1698700000", 1_700_000_000).as_deref(),
            Some("2w ago")
        );
        let mut ro = sample_session("s1", "Readonly", "kanban", 1);
        ro.read_only_reason = Some("locked".into());
        assert_eq!(
            session_meta_line(&ro, 1_700_000_120),
            "1 msg · 2m ago · read-only"
        );
    }

    #[test]
    fn launch_composer_preserves_draft_on_failure_and_consumes_on_create() {
        assert_eq!(derive_launch_title("   \n  "), "New conversation");
        assert_eq!(
            derive_launch_title("## Summarize   today's commits\nmore details"),
            "Summarize today's commits"
        );
        let long =
            "Explain how Wayland frame callbacks interact with GTK4 list recycling in detail";
        assert!(derive_launch_title(long).ends_with('…'));

        let mut state = AppState::default();
        state.set_active_draft("  Ship command palette  ".into());
        assert_eq!(state.active_draft(), "  Ship command palette  ");
        let (title, prompt) = state.begin_launch_submit().expect("launch submit");
        assert_eq!(title, "Ship command palette");
        assert_eq!(prompt, "Ship command palette");
        assert!(state.creating);
        assert!(state.begin_launch_submit().is_none());

        // Disconnect cancels the pending launch without losing the typed draft.
        state.disconnect();
        assert!(!state.creating);
        assert!(state.pending_launch.is_none());
        assert_eq!(state.launch_draft, "  Ship command palette  ");

        let _ = state.begin_launch_submit().expect("retry launch submit");
        assert_eq!(
            state.take_pending_launch().as_deref(),
            Some("Ship command palette")
        );
        assert!(state.launch_draft.is_empty());
    }

    #[test]
    fn session_titles_and_deletion_keep_selection_and_runtime_mapping_consistent() {
        let mut state = AppState {
            sessions: vec![sample_session("stored-1", "New conversation", "tui", 0)],
            selected: Some("stored-1".into()),
            ..Default::default()
        };
        state.conversations.insert(
            "stored-1".into(),
            Conversation {
                runtime_id: Some("rt-1".into()),
                draft: "hello".into(),
                ..Default::default()
            },
        );
        assert_eq!(state.active_draft(), "hello");
        assert!(state.apply_session_title("rt-1", "  Hyprland Palette Design  "));
        assert_eq!(state.sessions[0].title, "Hyprland Palette Design");
        assert!(!state.apply_session_title("rt-1", "Hyprland Palette Design"));
        assert!(!state.apply_session_title("rt-1", "   "));
        assert!(!state.apply_session_title("unknown", "Other"));

        assert!(!state.remove_session("missing"));
        assert!(state.remove_session("stored-1"));
        assert!(state.selected.is_none());
        assert!(state.sessions.is_empty());
        assert!(state.conversations.is_empty());
    }

    #[test]
    fn command_palette_offers_contextual_actions_and_ranks_queries() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            sessions: vec![
                sample_session("s1", "Cloudflare Tunnel Setup", "tui", 6),
                sample_session("s2", "stock-market-snapshot", "cron", 2),
            ],
            selected: Some("s1".into()),
            list_has_more: true,
            ..Default::default()
        };
        state.conversations.insert(
            "s1".into(),
            Conversation {
                runtime_id: Some("rt-1".into()),
                ..Default::default()
            },
        );
        let initial = build_palette_entries(&state, "", 1_700_000_600);
        assert!(
            initial
                .iter()
                .any(|e| e.action == PaletteAction::SwitchModel)
        );
        assert!(
            initial
                .iter()
                .any(|e| e.action == PaletteAction::DeleteCurrent)
        );
        assert!(
            initial
                .iter()
                .any(|e| e.action == PaletteAction::LoadMoreSessions)
        );

        let tunnel = build_palette_entries(&state, "cloudflare", 1_700_000_600);
        assert_eq!(tunnel.len(), 1);
        assert_eq!(tunnel[0].action, PaletteAction::OpenSession("s1".into()));

        let model = build_palette_entries(&state, "switch model", 1_700_000_600);
        assert_eq!(model[0].action, PaletteAction::SwitchModel);
    }
}
