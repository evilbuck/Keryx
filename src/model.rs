use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;

pub use crate::studio::{
    CompressSummary, MAX_IMAGE_BYTES, MAX_STAGED_IMAGES, SkillSummary, SlashCandidate, StagedImage,
    WorkspaceCatalog, WorkspaceOption,
};

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
    pub parent_session_id: Option<String>,
    pub is_branch: bool,
    pub pinned: bool,
    pub preview: String,
    pub model: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionFilter {
    #[default]
    All,
    Chats,
    Auto,
    Pinned,
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
            Self::Pinned => session.pinned,
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
    let model = short_model_badge(&session.model);
    if !model.is_empty() {
        parts.push(model);
    }
    if let Some(age) = format_relative_age(&session.updated_at, now_epoch_secs) {
        parts.push(age);
    }
    if session.read_only_reason.is_some() {
        parts.push("read-only".into());
    }
    parts.join(" · ")
}

pub fn short_model_badge(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if tail.chars().count() <= 22 {
        tail.to_string()
    } else {
        let prefix: String = tail.chars().take(21).collect();
        format!("{prefix}…")
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewindMode {
    /// Drop the last user turn and its reply (`intent: "undo"`).
    Undo,
    /// Drop the last user turn and load its prompt into the composer for editing (`intent: "retry"`).
    EditRetry,
    /// Drop the last user turn and immediately re-submit the same prompt (`intent: "retry"`).
    Regenerate,
}

impl RewindMode {
    pub fn wire_intent(self) -> &'static str {
        match self {
            Self::Undo => "undo",
            Self::EditRetry | Self::Regenerate => "retry",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchHit {
    pub stored_id: String,
    pub title: String,
    pub source: String,
    pub model: String,
    pub role: String,
    pub snippet_plain: String,
    pub snippet_markup: String,
    pub updated_at: String,
    pub message_count: usize,
    pub pinned: bool,
    pub parent_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionMutation {
    Rename(String),
    SetPinned(bool),
    Archive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOutcome {
    Updated,
    ArchivedSelected,
    ArchivedOther,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptFind {
    pub open: bool,
    pub query: String,
    pub matches: Vec<usize>,
    pub active_pos: usize,
}

impl TranscriptFind {
    pub fn open_with(&mut self, query: &str, messages: &[ChatMessage]) {
        self.open = true;
        let trimmed = query.trim();
        if !trimmed.is_empty() {
            self.query = trimmed.to_string();
        }
        self.matches = crate::recall::find_transcript_matches(messages, &self.query);
        self.active_pos = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.matches.clear();
        self.active_pos = 0;
    }

    pub fn set_query(&mut self, query: String, messages: &[ChatMessage]) {
        self.query = query;
        self.recompute(messages);
    }

    pub fn recompute(&mut self, messages: &[ChatMessage]) {
        if !self.open || self.query.trim().is_empty() {
            self.matches.clear();
            self.active_pos = 0;
            return;
        }
        let prev = self.active_message_index();
        self.matches = crate::recall::find_transcript_matches(messages, &self.query);
        self.active_pos = prev
            .and_then(|idx| self.matches.iter().position(|&m| m == idx))
            .unwrap_or(0);
    }

    pub fn step(&mut self, delta: isize) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let len = self.matches.len() as isize;
        self.active_pos = (self.active_pos as isize + delta).rem_euclid(len) as usize;
        self.active_message_index()
    }

    pub fn active_message_index(&self) -> Option<usize> {
        if self.open {
            self.matches.get(self.active_pos).copied()
        } else {
            None
        }
    }

    pub fn match_state(&self, message_index: usize) -> (bool, bool) {
        if !self.open || self.matches.is_empty() {
            return (false, false);
        }
        let is_match = self.matches.binary_search(&message_index).is_ok();
        let is_active = self.active_message_index() == Some(message_index);
        (is_match, is_active)
    }

    pub fn status_label(&self) -> String {
        if self.query.trim().is_empty() {
            "Type to find".into()
        } else if self.matches.is_empty() {
            "0 matches".into()
        } else {
            format!("{} / {}", self.active_pos + 1, self.matches.len())
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
        cwd: Option<String>,
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
        attachments: Vec<StagedImage>,
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
    /// Server-wide FTS5 search across all persisted conversations (`GET /api/sessions/search`).
    SearchSessions {
        query: String,
        request_seq: u64,
    },
    /// Server-persisted session curation (`PATCH /api/sessions/{id}`).
    MutateSession {
        stored_id: String,
        mutation: SessionMutation,
    },
    /// Fork a live or read-only conversation (`session.branch`, `session.branch_stored`, or seeded `session.create`).
    Branch {
        stored_id: String,
        runtime_id: Option<String>,
        parent_title: String,
        before_loaded: usize,
        visible_count: Option<usize>,
        custom_title: Option<String>,
        seed_fallback: Vec<SeedMessage>,
    },
    /// Rewind the last user turn and everything after it (`session.undo`).
    Undo {
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
    },
    /// Compact older turns on the server to free context (`session.compress`).
    Compress {
        stored_id: String,
        runtime_id: String,
    },
    /// Fetch remote workspace directories and enabled skills (`GET /api/chat/workspaces`, `GET /api/skills`).
    FetchStudio,
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
    /// Ephemeral side-question over a snapshot of the live conversation (`prompt.btw`).
    /// Never writes server history, alters turn alternation, or interrupts a busy turn.
    AskSide {
        runtime_id: String,
        client_id: u64,
        question: String,
    },
    Reconnect,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorTab {
    #[default]
    SideAsk,
    Trace,
    Outline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerIntent {
    Empty,
    Prompt(String),
    SideQuestion(String),
    Branch(Option<String>),
    Rewind(RewindMode),
    FindInTranscript(Option<String>),
    TogglePin,
    Rename(String),
    Archive,
    Compress,
    AttachImage,
    PickWorkspace(Option<String>),
}

pub fn parse_composer_input(raw: &str) -> ComposerIntent {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ComposerIntent::Empty;
    }
    if let Some(side) = parse_side_command(trimmed) {
        return side;
    }
    if let Some(timeline) = parse_timeline_command(trimmed) {
        return timeline;
    }
    if let Some(curation) = parse_curation_command(trimmed) {
        return curation;
    }
    if let Some(studio) = parse_studio_command(trimmed) {
        return studio;
    }
    ComposerIntent::Prompt(trimmed.to_string())
}

fn parse_studio_command(trimmed: &str) -> Option<ComposerIntent> {
    for cmd in ["/compress", "/compact"] {
        if strip_command_prefix(trimmed, cmd) == Some("") {
            return Some(ComposerIntent::Compress);
        }
    }
    for cmd in ["/attach", "/image"] {
        if strip_command_prefix(trimmed, cmd) == Some("") {
            return Some(ComposerIntent::AttachImage);
        }
    }
    for prefix in ["/workspace", "/cwd"] {
        if let Some(rest) = strip_command_prefix(trimmed, prefix) {
            let query = (!rest.is_empty()).then(|| rest.to_string());
            return Some(ComposerIntent::PickWorkspace(query));
        }
    }
    None
}

fn parse_curation_command(trimmed: &str) -> Option<ComposerIntent> {
    for prefix in ["/find", "/search"] {
        if let Some(rest) = strip_command_prefix(trimmed, prefix) {
            let query = (!rest.is_empty()).then(|| rest.to_string());
            return Some(ComposerIntent::FindInTranscript(query));
        }
    }
    for prefix in ["/rename", "/title"] {
        if let Some(rest) = strip_command_prefix(trimmed, prefix) {
            return Some(if rest.is_empty() {
                ComposerIntent::Empty
            } else {
                ComposerIntent::Rename(rest.to_string())
            });
        }
    }
    if strip_command_prefix(trimmed, "/pin") == Some("") {
        return Some(ComposerIntent::TogglePin);
    }
    if strip_command_prefix(trimmed, "/archive") == Some("") {
        return Some(ComposerIntent::Archive);
    }
    None
}

fn parse_side_command(trimmed: &str) -> Option<ComposerIntent> {
    for prefix in ["/btw", "/ask"] {
        if let Some(rest) = strip_command_prefix(trimmed, prefix) {
            return Some(if rest.is_empty() {
                ComposerIntent::Empty
            } else {
                ComposerIntent::SideQuestion(rest.to_string())
            });
        }
    }
    None
}

fn parse_timeline_command(trimmed: &str) -> Option<ComposerIntent> {
    for prefix in ["/branch", "/fork"] {
        if let Some(rest) = strip_command_prefix(trimmed, prefix) {
            let custom = (!rest.is_empty()).then(|| rest.to_string());
            return Some(ComposerIntent::Branch(custom));
        }
    }
    let rewinds = [
        ("/undo", RewindMode::Undo),
        ("/retry", RewindMode::EditRetry),
        ("/regen", RewindMode::Regenerate),
        ("/regenerate", RewindMode::Regenerate),
    ];
    for (cmd, mode) in rewinds {
        if strip_command_prefix(trimmed, cmd) == Some("") {
            return Some(ComposerIntent::Rewind(mode));
        }
    }
    None
}

fn strip_command_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let tail = text.strip_prefix(prefix)?;
    if tail.is_empty() {
        return Some("");
    }
    tail.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| tail.trim())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideQuestionStatus {
    Pending,
    Answered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideQuestion {
    pub client_id: u64,
    pub task_id: Option<String>,
    pub question: String,
    pub answer: String,
    pub status: SideQuestionStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Finished,
    RiskFlagged(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolTrace {
    pub tool_id: String,
    pub name: String,
    pub detail: String,
    pub progress_pct: Option<u8>,
    pub duration_s: Option<f64>,
    pub state: ToolState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolEventUpdate {
    Started {
        tool_id: String,
        name: String,
        detail: String,
    },
    Progress {
        tool_id: String,
        name: String,
        percent: Option<u8>,
        message: String,
    },
    Risk {
        tool_id: String,
        name: String,
        risk: String,
    },
    Completed {
        tool_id: String,
        name: String,
        duration_s: Option<f64>,
        summary: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionTelemetry {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    pub context_used: Option<u64>,
    pub context_max: Option<u64>,
    pub context_percent: Option<u8>,
    pub tokens_per_sec: Option<f64>,
    pub avg_latency_s: Option<f64>,
    pub api_calls: u64,
    pub compressions: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cwd: String,
    pub branch: String,
}

impl SessionTelemetry {
    pub fn has_usage(&self) -> bool {
        self.total_tokens > 0
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.context_used.is_some()
            || self.api_calls > 0
    }

    pub fn merge(&mut self, next: Self) {
        self.merge_tokens(&next);
        self.merge_context(&next);
        self.merge_rates(&next);
        if !next.cwd.is_empty() {
            self.cwd = next.cwd;
        }
        if !next.branch.is_empty() {
            self.branch = next.branch;
        }
    }

    fn merge_tokens(&mut self, next: &Self) {
        if next.input_tokens > 0 {
            self.input_tokens = next.input_tokens;
        }
        if next.output_tokens > 0 {
            self.output_tokens = next.output_tokens;
        }
        if next.reasoning_tokens > 0 {
            self.reasoning_tokens = next.reasoning_tokens;
        }
        if next.cache_read_tokens > 0 {
            self.cache_read_tokens = next.cache_read_tokens;
        }
        if next.total_tokens > 0 {
            self.total_tokens = next.total_tokens;
        } else if self.total_tokens == 0 {
            self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
        }
    }

    fn merge_context(&mut self, next: &Self) {
        if next.context_used.is_some() {
            self.context_used = next.context_used;
        }
        if next.context_max.is_some() {
            self.context_max = next.context_max;
        }
        if next.context_percent.is_some() {
            self.context_percent = next.context_percent;
        } else if let (Some(used), Some(max)) = (self.context_used, self.context_max)
            && max > 0
        {
            self.context_percent = Some(percent_u8(used, max));
        }
    }

    fn merge_rates(&mut self, next: &Self) {
        if next.tokens_per_sec.is_some() {
            self.tokens_per_sec = next.tokens_per_sec;
        }
        if next.avg_latency_s.is_some() {
            self.avg_latency_s = next.avg_latency_s;
        }
        if next.api_calls > 0 {
            self.api_calls = next.api_calls;
        }
        if next.compressions.is_some() {
            self.compressions = next.compressions;
        }
        if next.cost_usd.is_some() {
            self.cost_usd = next.cost_usd;
        }
    }

    pub fn context_fraction(&self) -> Option<f64> {
        if let (Some(used), Some(max)) = (self.context_used, self.context_max)
            && max > 0
        {
            return Some((used as f64 / max as f64).clamp(0.0, 1.0));
        }
        self.context_percent
            .map(|pct| (f64::from(pct) / 100.0).clamp(0.0, 1.0))
    }

    pub fn context_summary(&self) -> String {
        if let (Some(used), Some(max)) = (self.context_used, self.context_max)
            && max > 0
        {
            let pct = self
                .context_percent
                .unwrap_or_else(|| percent_u8(used, max));
            return format!(
                "{} / {} ({pct}%)",
                format_compact_tokens(used),
                format_compact_tokens(max)
            );
        }
        if let Some(used) = self.context_used {
            return format!("{} context tokens", format_compact_tokens(used));
        }
        if self.total_tokens > 0 {
            return format!("{} total tokens", format_compact_tokens(self.total_tokens));
        }
        "Waiting for first inference…".into()
    }

    pub fn header_badge(&self) -> Option<String> {
        if let Some(pct) = self.context_percent {
            return Some(format!("{pct}% ctx"));
        }
        if self.total_tokens > 0 {
            return Some(format!("{} tok", format_compact_tokens(self.total_tokens)));
        }
        None
    }
}

fn percent_u8(used: u64, max: u64) -> u8 {
    if max == 0 {
        return 0;
    }
    ((used.saturating_mul(100)) / max).min(100) as u8
}

pub fn format_compact_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

pub fn parse_telemetry_payload(value: &serde_json::Value) -> Option<SessionTelemetry> {
    let core = value
        .get("usage")
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    if !core.is_object() {
        return None;
    }
    let mut out = extract_token_counters(core, value);
    extract_context_counters(&mut out, core, value);
    extract_rate_counters(&mut out, core, value);
    out.has_usage().then_some(out)
}

pub fn parse_live_info_telemetry(result: &serde_json::Value) -> Option<SessionTelemetry> {
    let info = result
        .get("info")
        .filter(|v| v.is_object())
        .unwrap_or(result);
    if !info.is_object() {
        return None;
    }
    let mut out = info
        .get("usage")
        .and_then(parse_telemetry_payload)
        .unwrap_or_default();
    out.cwd = json_str(info, &["cwd"]).unwrap_or_default();
    out.branch = json_str(info, &["branch", "git_branch"]).unwrap_or_default();
    (out.has_usage() || !out.cwd.is_empty() || !out.branch.is_empty()).then_some(out)
}

fn extract_token_counters(core: &serde_json::Value, outer: &serde_json::Value) -> SessionTelemetry {
    let input = first_u64(
        core,
        outer,
        &["input", "input_tokens", "prompt_tokens", "prompt"],
    )
    .unwrap_or(0);
    let output = first_u64(
        core,
        outer,
        &["output", "output_tokens", "completion_tokens", "completion"],
    )
    .unwrap_or(0);
    let total = first_u64(core, outer, &["total", "total_tokens"])
        .unwrap_or_else(|| input.saturating_add(output));
    SessionTelemetry {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: first_u64(core, outer, &["reasoning", "reasoning_tokens"]).unwrap_or(0),
        cache_read_tokens: first_u64(core, outer, &["cache_read", "cache_read_tokens"])
            .unwrap_or(0),
        total_tokens: total,
        ..Default::default()
    }
}

fn extract_context_counters(
    out: &mut SessionTelemetry,
    core: &serde_json::Value,
    outer: &serde_json::Value,
) {
    out.context_used = first_u64(
        core,
        outer,
        &["context_used", "context_tokens", "last_prompt_tokens"],
    );
    out.context_max = first_u64(
        core,
        outer,
        &["context_max", "context_length", "config_context_length"],
    )
    .filter(|&max| max > 0);
    out.context_percent = first_u64(core, outer, &["context_percent"])
        .map(|pct| pct.min(100) as u8)
        .or_else(|| {
            out.context_used
                .zip(out.context_max)
                .map(|(used, max)| percent_u8(used, max))
        });
}

fn extract_rate_counters(
    out: &mut SessionTelemetry,
    core: &serde_json::Value,
    outer: &serde_json::Value,
) {
    out.tokens_per_sec = first_f64(core, outer, &["avg_tps", "tokens_per_s"]);
    out.avg_latency_s = first_f64(core, outer, &["avg_latency_s"]);
    out.api_calls = first_u64(core, outer, &["calls", "api_calls"]).unwrap_or(0);
    out.compressions = first_u64(core, outer, &["compressions", "compression_count"]);
    out.cost_usd = first_f64(core, outer, &["cost_usd", "actual_cost_usd"]);
}

fn first_u64(
    primary: &serde_json::Value,
    fallback: &serde_json::Value,
    keys: &[&str],
) -> Option<u64> {
    keys.iter().find_map(|key| {
        primary
            .get(*key)
            .and_then(serde_json::Value::as_u64)
            .or_else(|| fallback.get(*key).and_then(serde_json::Value::as_u64))
    })
}

fn first_f64(
    primary: &serde_json::Value,
    fallback: &serde_json::Value,
    keys: &[&str],
) -> Option<f64> {
    keys.iter().find_map(|key| {
        primary
            .get(*key)
            .and_then(serde_json::Value::as_f64)
            .or_else(|| fallback.get(*key).and_then(serde_json::Value::as_f64))
            .filter(|v| v.is_finite() && *v >= 0.0)
    })
}

fn json_str(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineEntry {
    pub turn_number: usize,
    pub message_index: usize,
    pub end_message_index: usize,
    pub title: String,
    pub meta: String,
    pub code_blocks: usize,
}

pub fn build_outline(messages: &[ChatMessage]) -> Vec<OutlineEntry> {
    let user_positions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| (msg.role == Role::User).then_some(idx))
        .collect();
    user_positions
        .iter()
        .enumerate()
        .map(|(turn_idx, &start)| {
            let end = user_positions
                .get(turn_idx + 1)
                .copied()
                .unwrap_or(messages.len());
            outline_for_slice(
                turn_idx + 1,
                start,
                end.saturating_sub(1),
                &messages[start..end],
            )
        })
        .collect()
}

fn outline_for_slice(
    turn_number: usize,
    message_index: usize,
    end_message_index: usize,
    slice: &[ChatMessage],
) -> OutlineEntry {
    let title = slice
        .first()
        .map(|m| derive_launch_title(&m.text))
        .unwrap_or_else(|| format!("Turn {turn_number}"));
    let mut tools = 0usize;
    let mut code_blocks = 0usize;
    let mut replied = false;
    for msg in slice.iter().skip(1) {
        if msg.role == Role::Tool {
            tools += 1;
        } else if msg.role == Role::Assistant {
            replied = true;
            code_blocks += count_fenced_blocks(&msg.text);
        }
    }
    OutlineEntry {
        turn_number,
        message_index,
        end_message_index,
        title,
        meta: format_outline_meta(replied, tools, code_blocks),
        code_blocks,
    }
}

pub fn is_branchable_message(message: &ChatMessage) -> bool {
    matches!(message.role, Role::User | Role::Assistant) && !message.text.trim().is_empty()
}

pub fn count_visible_branch_messages(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|message| is_branchable_message(message))
        .count()
}

/// Server-side `session.branch` `count` argument for forking up through `index` (inclusive).
/// Returns `None` when `index` includes all visible messages (meaning a whole-session branch).
pub fn visible_branch_count_through(messages: &[ChatMessage], index: usize) -> Option<usize> {
    if messages.is_empty() {
        return None;
    }
    let capped = index.min(messages.len() - 1);
    let prefix = count_visible_branch_messages(&messages[..=capped]);
    let total = count_visible_branch_messages(messages);
    (prefix > 0 && prefix < total).then_some(prefix)
}

pub fn seed_messages_through(
    messages: &[ChatMessage],
    end_inclusive: Option<usize>,
) -> Vec<SeedMessage> {
    let slice = match end_inclusive {
        Some(idx) if !messages.is_empty() => &messages[..=idx.min(messages.len() - 1)],
        _ => messages,
    };
    slice
        .iter()
        .filter(|m| is_branchable_message(m))
        .map(|m| SeedMessage {
            role: if m.role == Role::User {
                "user".into()
            } else {
                "assistant".into()
            },
            content: m.text.clone(),
        })
        .collect()
}

/// Identifies `(last_user_id, last_assistant_after_user_id, last_user_text)` for inline rewind actions.
pub fn last_turn_targets(
    messages: &[ChatMessage],
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(user_idx) = messages
        .iter()
        .rposition(|m| m.role == Role::User && !m.text.trim().is_empty())
    else {
        return (None, None, None);
    };
    let user = &messages[user_idx];
    let assistant_id = messages[user_idx + 1..]
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant && !m.text.trim().is_empty())
        .map(|m| m.id.clone());
    (Some(user.id.clone()), assistant_id, Some(user.text.clone()))
}

fn count_fenced_blocks(markdown: &str) -> usize {
    let fences = markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    fences / 2
}

fn format_outline_meta(replied: bool, tools: usize, code_blocks: usize) -> String {
    let mut parts = Vec::new();
    if tools > 0 {
        parts.push(if tools == 1 {
            "1 tool".into()
        } else {
            format!("{tools} tools")
        });
    }
    if code_blocks > 0 {
        parts.push(if code_blocks == 1 {
            "1 code block".into()
        } else {
            format!("{code_blocks} code blocks")
        });
    }
    if parts.is_empty() {
        parts.push(if replied {
            "replied".into()
        } else {
            "awaiting reply".into()
        });
    }
    parts.join(" · ")
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
    Branched {
        session: SessionSummary,
        parent_title: String,
        runtime_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        model: Option<ModelIdentity>,
    },
    Undone {
        stored_id: String,
        runtime_id: String,
        removed: usize,
        messages: Vec<ChatMessage>,
        before: usize,
        mode: RewindMode,
        prompt_text: String,
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
    /// Ephemeral `/btw` side-question accepted by the gateway (`prompt.btw`).
    SideStarted {
        runtime_id: String,
        client_id: u64,
        task_id: String,
    },
    /// Ephemeral `/btw` side-question answered (`btw.complete`).
    SideComplete {
        runtime_id: String,
        task_id: String,
        question: String,
        text: String,
    },
    /// Ephemeral `/btw` side-question rejected or timed out without affecting the main turn.
    SideFailed {
        runtime_id: String,
        client_id: u64,
        message: String,
    },
    /// Streamed or completed model reasoning (`reasoning.delta`, `thinking.delta`, `reasoning.available`).
    Reasoning {
        runtime_id: String,
        text: String,
        replace: bool,
    },
    /// Live tool execution progress (`tool.start`, `tool.progress`, `tool.output_risk`, `tool.complete`).
    ToolTraceEvent {
        runtime_id: String,
        update: ToolEventUpdate,
    },
    /// Session token, throughput, context-window, and workspace telemetry.
    Telemetry {
        runtime_id: String,
        telemetry: SessionTelemetry,
    },
    /// Server-wide FTS5 search hits (`GET /api/sessions/search`).
    SearchResults {
        query: String,
        request_seq: u64,
        hits: Vec<SearchHit>,
    },
    /// Confirmed session curation (`PATCH /api/sessions/{id}`).
    SessionMutated {
        stored_id: String,
        mutation: SessionMutation,
    },
    /// Remote workspace catalog (`GET /api/chat/workspaces`).
    Workspaces {
        catalog: WorkspaceCatalog,
    },
    /// Enabled remote skills catalog (`GET /api/skills`).
    Skills {
        skills: Vec<SkillSummary>,
    },
    /// Context compaction result (`session.compress`).
    Compressed {
        stored_id: String,
        runtime_id: String,
        messages: Option<Vec<ChatMessage>>,
        before: usize,
        summary: CompressSummary,
    },
    Error {
        operation: String,
        session_id: Option<String>,
        message: String,
    },
}

const MAX_REASONING_CHARS: usize = 24_000;
const MAX_TOOL_TRACES: usize = 80;
const MAX_SIDE_QUESTIONS: usize = 40;

#[derive(Default)]
pub struct Conversation {
    pub runtime_id: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub draft: String,
    pub attachments: Vec<StagedImage>,
    pub status: TurnStatus,
    pub activity: String,
    pub read_only_reason: Option<String>,
    pub interaction: Option<InteractionRequest>,
    pub loaded: bool,
    pub history_before: usize,
    pub history_loading: bool,
    pub rewinding: bool,
    pub revision: u64,
    pub inspector_revision: u64,
    pub model: ModelState,
    pub reasoning: String,
    pub tools: Vec<ToolTrace>,
    pub side_questions: Vec<SideQuestion>,
    pub telemetry: SessionTelemetry,
}

impl Conversation {
    pub fn record_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        // Display rows include optimistic and streamed messages. They must not
        // move the absolute server-row cursor used by oldest-first paging.
        self.revision += 1;
        self.inspector_revision += 1;
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
            self.inspector_revision += 1;
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
        self.inspector_revision += 1;
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
        if matches!(operation, "branch" | "undo" | "compress") {
            self.rewinding = false;
            self.activity.clear();
        }
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

    pub fn clear_turn_reasoning(&mut self) {
        if !self.reasoning.is_empty() {
            self.reasoning.clear();
            self.inspector_revision += 1;
        }
    }

    pub fn append_reasoning(&mut self, text: &str, replace: bool) {
        if text.is_empty() && !replace {
            return;
        }
        if replace {
            self.reasoning = text.to_string();
        } else {
            self.reasoning.push_str(text);
        }
        trim_front_chars(&mut self.reasoning, MAX_REASONING_CHARS);
        self.inspector_revision += 1;
    }

    pub fn apply_tool_event(&mut self, update: ToolEventUpdate) {
        upsert_tool_trace(&mut self.tools, update);
        if self.tools.len() > MAX_TOOL_TRACES {
            let overflow = self.tools.len() - MAX_TOOL_TRACES;
            self.tools.drain(0..overflow);
        }
        self.inspector_revision += 1;
    }

    pub fn apply_telemetry(&mut self, incoming: SessionTelemetry) {
        self.telemetry.merge(incoming);
        self.inspector_revision += 1;
    }

    pub fn begin_side_question(&mut self, client_id: u64, question: String) {
        self.side_questions.push(SideQuestion {
            client_id,
            task_id: None,
            question,
            answer: String::new(),
            status: SideQuestionStatus::Pending,
        });
        if self.side_questions.len() > MAX_SIDE_QUESTIONS {
            let overflow = self.side_questions.len() - MAX_SIDE_QUESTIONS;
            self.side_questions.drain(0..overflow);
        }
        self.inspector_revision += 1;
    }

    pub fn bind_side_task(&mut self, client_id: u64, task_id: String) {
        if let Some(entry) = self
            .side_questions
            .iter_mut()
            .find(|q| q.client_id == client_id)
        {
            entry.task_id = Some(task_id);
            self.inspector_revision += 1;
        }
    }

    pub fn complete_side_question(&mut self, task_id: &str, question: &str, text: String) {
        let status = side_answer_status(&text);
        if let Some(idx) = find_side_question_index(&self.side_questions, task_id, question) {
            let entry = &mut self.side_questions[idx];
            if !task_id.is_empty() {
                entry.task_id = Some(task_id.to_string());
            }
            entry.answer = text;
            entry.status = status;
        } else {
            self.push_unsolicited_side(task_id, question, text, status);
        }
        self.inspector_revision += 1;
    }

    fn push_unsolicited_side(
        &mut self,
        task_id: &str,
        question: &str,
        answer: String,
        status: SideQuestionStatus,
    ) {
        self.side_questions.push(SideQuestion {
            client_id: 0,
            task_id: (!task_id.is_empty()).then(|| task_id.to_string()),
            question: if question.is_empty() {
                "Side question".into()
            } else {
                question.to_string()
            },
            answer,
            status,
        });
        if self.side_questions.len() > MAX_SIDE_QUESTIONS {
            self.side_questions.remove(0);
        }
    }

    pub fn fail_side_question(&mut self, client_id: u64, message: String) {
        if let Some(entry) = self
            .side_questions
            .iter_mut()
            .find(|q| q.client_id == client_id && q.status == SideQuestionStatus::Pending)
        {
            entry.answer = message;
            entry.status = SideQuestionStatus::Failed;
            self.inspector_revision += 1;
        }
    }

    pub fn fail_pending_side_questions(&mut self, message: &str) {
        let mut changed = false;
        for entry in &mut self.side_questions {
            if entry.status == SideQuestionStatus::Pending {
                entry.answer = message.to_string();
                entry.status = SideQuestionStatus::Failed;
                changed = true;
            }
        }
        if changed {
            self.inspector_revision += 1;
        }
    }
}

fn trim_front_chars(buffer: &mut String, max_chars: usize) {
    let total = buffer.chars().count();
    if total <= max_chars {
        return;
    }
    let drop_count = total - max_chars;
    if let Some((byte_idx, _)) = buffer.char_indices().nth(drop_count) {
        buffer.drain(..byte_idx);
    }
}

fn side_answer_status(text: &str) -> SideQuestionStatus {
    if text.trim_start().starts_with("error:") {
        SideQuestionStatus::Failed
    } else {
        SideQuestionStatus::Answered
    }
}

fn find_side_question_index(
    items: &[SideQuestion],
    task_id: &str,
    question: &str,
) -> Option<usize> {
    if !task_id.is_empty()
        && let Some(idx) = items
            .iter()
            .rposition(|q| q.task_id.as_deref() == Some(task_id))
    {
        return Some(idx);
    }
    items.iter().position(|q| {
        q.status == SideQuestionStatus::Pending
            && q.task_id.is_none()
            && (question.is_empty() || q.question == question)
    })
}

fn upsert_tool_trace(tools: &mut Vec<ToolTrace>, update: ToolEventUpdate) {
    match update {
        ToolEventUpdate::Started {
            tool_id,
            name,
            detail,
        } => upsert_started_tool(tools, tool_id, name, detail),
        ToolEventUpdate::Progress {
            tool_id,
            name,
            percent,
            message,
        } => {
            let entry = ensure_tool_entry(tools, &tool_id, &name);
            entry.progress_pct = percent.or(entry.progress_pct);
            if !message.is_empty() {
                entry.detail = message;
            }
        }
        ToolEventUpdate::Risk {
            tool_id,
            name,
            risk,
        } => {
            let entry = ensure_tool_entry(tools, &tool_id, &name);
            entry.state = ToolState::RiskFlagged(risk);
        }
        ToolEventUpdate::Completed {
            tool_id,
            name,
            duration_s,
            summary,
        } => finish_tool_entry(tools, &tool_id, &name, duration_s, summary),
    }
}

fn upsert_started_tool(tools: &mut Vec<ToolTrace>, tool_id: String, name: String, detail: String) {
    if !tool_id.is_empty()
        && let Some(existing) = tools.iter_mut().rev().find(|t| t.tool_id == tool_id)
    {
        if !name.is_empty() {
            existing.name = name;
        }
        if !detail.is_empty() {
            existing.detail = detail;
        }
        return;
    }
    tools.push(ToolTrace {
        tool_id,
        name: if name.is_empty() { "tool".into() } else { name },
        detail,
        progress_pct: None,
        duration_s: None,
        state: ToolState::Running,
    });
}

fn finish_tool_entry(
    tools: &mut Vec<ToolTrace>,
    tool_id: &str,
    name: &str,
    duration_s: Option<f64>,
    summary: String,
) {
    let entry = ensure_tool_entry(tools, tool_id, name);
    entry.duration_s = duration_s.or(entry.duration_s);
    entry.progress_pct = Some(100);
    if !summary.is_empty() {
        entry.detail = summary;
    }
    if !matches!(entry.state, ToolState::RiskFlagged(_)) {
        entry.state = ToolState::Finished;
    }
}

fn ensure_tool_entry<'a>(
    tools: &'a mut Vec<ToolTrace>,
    tool_id: &str,
    name: &str,
) -> &'a mut ToolTrace {
    let pos = if !tool_id.is_empty() {
        tools.iter().rposition(|t| t.tool_id == tool_id)
    } else {
        tools
            .iter()
            .rposition(|t| t.state == ToolState::Running && (name.is_empty() || t.name == name))
    };
    if let Some(idx) = pos {
        let entry = &mut tools[idx];
        if !name.is_empty() && entry.name == "tool" {
            entry.name = name.to_string();
        }
        return entry;
    }
    tools.push(ToolTrace {
        tool_id: tool_id.to_string(),
        name: if name.is_empty() {
            "tool".into()
        } else {
            name.to_string()
        },
        detail: String::new(),
        progress_pct: None,
        duration_s: None,
        state: ToolState::Running,
    });
    tools.last_mut().expect("inserted tool trace")
}

/// Renders a transcript as portable Markdown, one `##` heading per turn.
pub fn transcript_markdown(title: &str, messages: &[ChatMessage]) -> String {
    let mut out = format!("# {}\n", title.trim());
    for m in messages {
        let label = match m.role {
            Role::User => "You",
            Role::Assistant => "Hermes",
            Role::Tool => "Tool",
            Role::System => "System",
        };
        out.push_str(&format!("\n## {label}\n\n{}\n", m.text.trim_end()));
    }
    out
}

/// Derives a safe `.md` file name from a session title.
pub fn export_file_name(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "conversation.md".into()
    } else {
        format!("{slug}.md")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewChat,
    SwitchWorkspace,
    AttachImage,
    CompressCurrent,
    BranchCurrent,
    RewindLast(RewindMode),
    SwitchModel,
    ToggleSidebar,
    ToggleInspector,
    OpenInspector(InspectorTab),
    FocusFilter,
    FindInTranscript,
    CopyTranscript,
    ExportTranscript,
    TogglePinCurrent,
    RenameCurrent,
    ArchiveCurrent,
    FilterSessions(SessionFilter),
    ReloadHistory,
    LoadMoreSessions,
    Reconnect,
    DeleteCurrent,
    OpenSession(String),
    OpenSearchHit { stored_id: String, query: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    pub action: PaletteAction,
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    pub subtitle_markup: Option<String>,
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
            subtitle_markup: None,
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
    pub branch_parents: HashMap<String, (String, String)>,
    pub selected: Option<String>,
    pub connection: Option<ConnectionState>,
    pub list_has_more: bool,
    pub list_loading: bool,
    pub creating: bool,
    pub notice: String,
    pub catalog: Catalog,
    pub workspaces: WorkspaceCatalog,
    pub skills: Vec<SkillSummary>,
    pub launch_cwd: Option<String>,
    pub launch_attachments: Vec<StagedImage>,
    pub session_filter: SessionFilter,
    pub session_query: String,
    pub launch_draft: String,
    pub pending_launch: Option<String>,
    pub inspector_open: bool,
    pub inspector_tab: InspectorTab,
    pub next_side_id: u64,
    pub next_attachment_id: u64,
    pub remote_search_seq: u64,
    pub remote_search_loading: bool,
    pub remote_search_query: String,
    pub remote_search_hits: Vec<SearchHit>,
    pub transcript_find: TranscriptFind,
    pub pending_find_query: Option<String>,
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
        self.sync_branch_flags();
    }

    pub fn record_branch(&mut self, child_id: &str, parent_id: &str, parent_title: &str) {
        if child_id.is_empty() || parent_id.is_empty() {
            return;
        }
        self.branch_parents.insert(
            child_id.to_string(),
            (parent_id.to_string(), fallback_session_title(parent_title)),
        );
        self.sync_branch_flags();
    }

    /// Recompute `is_branch` on loaded session summaries. Auto-compressed predecessors are
    /// hidden by `GET /api/sessions`, so only true forks have a visible parent row or `(branch)` title.
    pub fn sync_branch_flags(&mut self) {
        let known_ids: std::collections::HashSet<String> =
            self.sessions.iter().map(|s| s.stored_id.clone()).collect();
        for session in &mut self.sessions {
            session.is_branch = self.branch_parents.contains_key(&session.stored_id)
                || session.parent_session_id.as_ref().is_some_and(|pid| {
                    pid != &session.stored_id
                        && (known_ids.contains(pid) || session.title.ends_with(" (branch)"))
                });
        }
    }

    pub fn parent_for(&self, stored_id: &str) -> Option<(String, String)> {
        if let Some((pid, saved_title)) = self.branch_parents.get(stored_id) {
            let live_title = self
                .sessions
                .iter()
                .find(|s| s.stored_id == *pid)
                .map(|s| fallback_session_title(&s.title))
                .unwrap_or_else(|| saved_title.clone());
            return Some((pid.clone(), live_title));
        }
        let session = self.sessions.iter().find(|s| s.stored_id == stored_id)?;
        let pid = session.parent_session_id.as_ref()?;
        if pid == stored_id {
            return None;
        }
        if let Some(parent) = self.sessions.iter().find(|s| &s.stored_id == pid) {
            return Some((pid.clone(), fallback_session_title(&parent.title)));
        }
        session.title.ends_with(" (branch)").then(|| {
            let stripped = session.title.trim_end_matches(" (branch)");
            (pid.clone(), fallback_session_title(stripped))
        })
    }

    pub fn branches_of(&self, stored_id: &str) -> Vec<&SessionSummary> {
        self.sessions
            .iter()
            .filter(|s| {
                s.stored_id != stored_id
                    && (s.parent_session_id.as_deref() == Some(stored_id)
                        || self
                            .branch_parents
                            .get(&s.stored_id)
                            .is_some_and(|(pid, _)| pid == stored_id))
            })
            .collect()
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

    pub fn pinned_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.pinned).count()
    }

    pub fn is_selected_pinned(&self) -> bool {
        self.selected
            .as_deref()
            .and_then(|id| self.sessions.iter().find(|s| s.stored_id == id))
            .is_some_and(|s| s.pinned)
    }

    /// Indices into `self.sessions` matching the active filter and sidebar search query.
    /// Pinned conversations float to the top while preserving server recency within each group.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let tokens = query_tokens(&self.session_query);
        let filtered = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| self.session_filter.matches(s));
        if tokens.is_empty() {
            let mut indices: Vec<(bool, usize)> =
                filtered.map(|(index, s)| (!s.pinned, index)).collect();
            indices.sort_by_key(|&(unpinned, index)| (unpinned, index));
            return indices.into_iter().map(|(_, index)| index).collect();
        }
        let mut ranked: Vec<(usize, bool, usize)> = filtered
            .filter_map(|(index, s)| {
                session_search_score(s, &tokens).map(|score| (score, !s.pinned, index))
            })
            .collect();
        ranked.sort_by_key(|&(score, unpinned, index)| (score, unpinned, index));
        ranked.into_iter().map(|(_, _, index)| index).collect()
    }

    pub fn begin_remote_search(&mut self, raw_query: &str) -> Option<(u64, String)> {
        let trimmed = raw_query.trim();
        self.remote_search_seq = self.remote_search_seq.saturating_add(1);
        if trimmed.chars().count() < 2 {
            self.remote_search_loading = false;
            self.remote_search_query.clear();
            self.remote_search_hits.clear();
            return None;
        }
        self.remote_search_loading = true;
        Some((self.remote_search_seq, trimmed.to_string()))
    }

    pub fn apply_search_results(
        &mut self,
        query: String,
        request_seq: u64,
        mut hits: Vec<SearchHit>,
    ) -> bool {
        if request_seq != self.remote_search_seq {
            return false;
        }
        for hit in &mut hits {
            if self
                .sessions
                .iter()
                .any(|s| s.stored_id == hit.stored_id && s.pinned)
            {
                hit.pinned = true;
            }
        }
        self.remote_search_loading = false;
        self.remote_search_query = query;
        self.remote_search_hits = hits;
        true
    }

    pub fn has_active_remote_search(&self, query: &str) -> bool {
        let trimmed = query.trim();
        trimmed.chars().count() >= 2 && self.remote_search_query.eq_ignore_ascii_case(trimmed)
    }

    /// Server FTS5 hits not already present in the locally-loaded `self.sessions` slice.
    pub fn remote_only_sidebar_hits(&self) -> Vec<&SearchHit> {
        if !self.has_active_remote_search(&self.session_query) {
            return Vec::new();
        }
        let known: std::collections::HashSet<&str> =
            self.sessions.iter().map(|s| s.stored_id.as_str()).collect();
        self.remote_search_hits
            .iter()
            .filter(|hit| !known.contains(hit.stored_id.as_str()))
            .filter(|hit| sidebar_filter_matches_hit(self.session_filter, hit))
            .collect()
    }

    pub fn apply_mutation(
        &mut self,
        stored_id: &str,
        mutation: &SessionMutation,
    ) -> MutationOutcome {
        match mutation {
            SessionMutation::Rename(title) => {
                self.apply_rename(stored_id, title);
                MutationOutcome::Updated
            }
            SessionMutation::SetPinned(pinned) => {
                self.apply_pinned(stored_id, *pinned);
                MutationOutcome::Updated
            }
            SessionMutation::Archive => {
                self.remote_search_hits.retain(|h| h.stored_id != stored_id);
                if self.remove_session(stored_id) {
                    self.transcript_find.close();
                    self.pending_find_query = None;
                    MutationOutcome::ArchivedSelected
                } else {
                    MutationOutcome::ArchivedOther
                }
            }
        }
    }

    fn apply_rename(&mut self, stored_id: &str, title: &str) {
        let clean = title.trim();
        if clean.is_empty() {
            return;
        }
        if let Some(session) = self.sessions.iter_mut().find(|s| s.stored_id == stored_id) {
            session.title = clean.to_string();
        }
        for hit in &mut self.remote_search_hits {
            if hit.stored_id == stored_id {
                hit.title = clean.to_string();
            }
        }
        self.sync_branch_flags();
    }

    fn apply_pinned(&mut self, stored_id: &str, pinned: bool) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.stored_id == stored_id) {
            session.pinned = pinned;
        }
        for hit in &mut self.remote_search_hits {
            if hit.stored_id == stored_id {
                hit.pinned = pinned;
            }
        }
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
    /// Keeps `launch_draft` and `launch_attachments` intact until `Created` succeeds so a failure never loses input.
    pub fn begin_launch_submit(&mut self) -> Option<(String, String)> {
        if self.selected.is_some() || self.creating || self.pending_launch.is_some() {
            return None;
        }
        let trimmed = self.launch_draft.trim().to_string();
        if trimmed.is_empty() && self.launch_attachments.is_empty() {
            return None;
        }
        let title =
            crate::studio::derive_multimodal_launch_title(&trimmed, &self.launch_attachments);
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

    pub fn take_pending_launch_bundle(&mut self) -> Option<(String, Vec<StagedImage>)> {
        let prompt = self.take_pending_launch()?;
        let attachments = std::mem::take(&mut self.launch_attachments);
        Some((prompt, attachments))
    }

    pub fn take_launch_draft_for_blank_chat(&mut self) -> String {
        if self.pending_launch.is_some() {
            return String::new();
        }
        std::mem::take(&mut self.launch_draft)
    }

    pub fn take_launch_attachments_for_blank_chat(&mut self) -> Vec<StagedImage> {
        if self.pending_launch.is_some() {
            return Vec::new();
        }
        std::mem::take(&mut self.launch_attachments)
    }

    pub fn allocate_attachment_id(&mut self) -> u64 {
        self.next_attachment_id = self.next_attachment_id.saturating_add(1);
        self.next_attachment_id
    }

    pub fn active_attachments(&self) -> &[StagedImage] {
        match &self.selected {
            Some(id) => self
                .conversations
                .get(id)
                .map_or(&[], |c| c.attachments.as_slice()),
            None => self.launch_attachments.as_slice(),
        }
    }

    pub fn stage_active_images(&mut self, incoming: Vec<StagedImage>) -> Result<usize, String> {
        if incoming.is_empty() {
            return Ok(0);
        }
        let target = match self.selected.clone() {
            Some(id) => &mut self.conversations.entry(id).or_default().attachments,
            None => &mut self.launch_attachments,
        };
        let available = MAX_STAGED_IMAGES.saturating_sub(target.len());
        if available == 0 {
            return Err(format!(
                "Up to {MAX_STAGED_IMAGES} images can be attached to one message."
            ));
        }
        let added = incoming.len().min(available);
        target.extend(incoming.into_iter().take(added));
        Ok(added)
    }

    pub fn remove_active_attachment(&mut self, id: u64) -> bool {
        let target = match self.selected.clone() {
            Some(sel) => {
                let Some(c) = self.conversations.get_mut(&sel) else {
                    return false;
                };
                &mut c.attachments
            }
            None => &mut self.launch_attachments,
        };
        let before = target.len();
        target.retain(|item| item.id != id);
        target.len() != before
    }

    pub fn take_conversation_attachments(&mut self, stored_id: &str) -> Vec<StagedImage> {
        self.conversations
            .get_mut(stored_id)
            .map(|c| std::mem::take(&mut c.attachments))
            .unwrap_or_default()
    }

    pub fn effective_active_cwd(&self) -> Option<&str> {
        if let Some(id) = &self.selected
            && let Some(c) = self.conversations.get(id)
            && !c.telemetry.cwd.trim().is_empty()
        {
            return Some(c.telemetry.cwd.as_str());
        }
        self.launch_cwd.as_deref()
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
        self.branch_parents.remove(stored_id);
        self.branch_parents.retain(|_, (pid, _)| pid != stored_id);
        self.sync_branch_flags();
        if self.selected.as_deref() == Some(stored_id) {
            self.selected = None;
            true
        } else {
            false
        }
    }

    pub fn allocate_side_id(&mut self) -> u64 {
        self.next_side_id = self.next_side_id.saturating_add(1);
        self.next_side_id
    }

    pub fn disconnect(&mut self) {
        self.cancel_pending_launch();
        self.remote_search_loading = false;
        for conversation in self.conversations.values_mut() {
            if conversation.status.busy() {
                conversation.status = TurnStatus::Unknown;
                conversation.activity = "Connection lost. This turn may have reached the server; it will not be sent again. Reconnect to reload server history.".into();
                conversation.interaction = None;
                conversation.revision += 1;
            }
            conversation.fail_pending_side_questions(
                "Connection lost before Hermes answered this side question.",
            );
            conversation.runtime_id = None;
            conversation.history_loading = false;
            conversation.rewinding = false;
        }
    }
}

fn session_search_score(session: &SessionSummary, tokens: &[String]) -> Option<usize> {
    let fork_tag = if session.is_branch { "branch fork" } else { "" };
    let pin_tag = if session.pinned { "pinned star" } else { "" };
    let key = normalize(&format!(
        "{} {} {} {} {} {fork_tag} {pin_tag}",
        session.title, session.preview, session.model, session.source, session.stored_id
    ));
    match_tokens(&key, tokens)
}

fn sidebar_filter_matches_hit(filter: SessionFilter, hit: &SearchHit) -> bool {
    match filter {
        SessionFilter::All => true,
        SessionFilter::Chats => classify_source(&hit.source) == SourceKind::Interactive,
        SessionFilter::Auto => classify_source(&hit.source) == SourceKind::Automated,
        SessionFilter::Pinned => hit.pinned,
    }
}

fn fallback_session_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "Untitled conversation".into()
    } else {
        trimmed.to_string()
    }
}

pub fn build_palette_entries(
    state: &AppState,
    query: &str,
    now_epoch_secs: i64,
) -> Vec<PaletteEntry> {
    let commands = palette_commands(state);
    let sessions = palette_sessions(state, query, now_epoch_secs);
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        let mut all = commands;
        all.extend(sessions);
        return all;
    }
    let mut ranked = rank_palette_slice(commands, &tokens, 0, false);
    ranked.extend(rank_palette_slice(sessions, &tokens, 1, true));
    ranked.sort_by_key(|&(score, kind_rank, index, _)| (score, kind_rank, index));
    let mut out: Vec<PaletteEntry> = ranked.into_iter().map(|(_, _, _, entry)| entry).collect();
    out.extend(palette_remote_hits(state, query, now_epoch_secs));
    out
}

fn rank_palette_slice(
    entries: Vec<PaletteEntry>,
    tokens: &[String],
    kind_rank: u8,
    include_fts_enriched: bool,
) -> Vec<(usize, u8, usize, PaletteEntry)> {
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if let Some(score) = match_tokens(&entry.search_key(), tokens) {
                Some((score, kind_rank, index, entry))
            } else if include_fts_enriched && entry.subtitle_markup.is_some() {
                Some((50 + index, kind_rank, index, entry))
            } else {
                None
            }
        })
        .collect()
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
    out.extend(palette_studio_commands(
        state,
        conversation,
        connected,
        writable,
    ));
    out.extend(palette_curation_commands(state, conversation, connected));
    out.extend(palette_timeline_commands(
        state,
        conversation,
        connected,
        writable,
    ));
    if writable && conversation.is_some_and(|c| !c.model.switching()) {
        out.push(PaletteEntry::command(
            PaletteAction::SwitchModel,
            "preferences-system-symbolic",
            "Switch model…",
            "Choose provider and model for this conversation",
            "Ctrl+M",
        ));
    }
    out.extend(palette_inspector_commands(state, writable));
    out.extend(palette_filter_commands(state));
    out.extend(palette_utility_commands(state, connected));
    out
}

fn palette_studio_commands(
    state: &AppState,
    conversation: Option<&Conversation>,
    connected: bool,
    writable: bool,
) -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    if connected && !state.creating {
        out.push(PaletteEntry::command(
            PaletteAction::SwitchWorkspace,
            "folder-symbolic",
            "Switch / launch in workspace…",
            "Choose a remote git repo or project directory (/workspace)",
            "Ctrl+Shift+W",
        ));
    }
    if connected && (state.selected.is_none() || writable) {
        out.push(PaletteEntry::command(
            PaletteAction::AttachImage,
            "insert-image-symbolic",
            "Attach image to prompt…",
            "Stage PNG, JPEG, WebP, or GIF for multimodal vision (/attach)",
            "Ctrl+U",
        ));
    }
    if writable
        && conversation.is_some_and(|c| !c.status.busy() && !c.rewinding && c.messages.len() >= 2)
    {
        out.push(PaletteEntry::command(
            PaletteAction::CompressCurrent,
            "view-restore-symbolic",
            "Compress conversation context",
            "Compact older turns on the server to free context window (/compress)",
            "",
        ));
    }
    out
}

fn palette_curation_commands(
    state: &AppState,
    conversation: Option<&Conversation>,
    connected: bool,
) -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    if conversation.is_some_and(|c| !c.messages.is_empty()) {
        out.push(PaletteEntry::command(
            PaletteAction::FindInTranscript,
            "edit-find-symbolic",
            "Find in current transcript…",
            "Jump through messages in the open conversation (/find)",
            "Ctrl+Shift+F",
        ));
    }
    if conversation.is_some_and(|c| !c.messages.is_empty()) {
        out.push(PaletteEntry::command(
            PaletteAction::CopyTranscript,
            "edit-copy-symbolic",
            "Copy conversation as Markdown",
            "Put the whole transcript on the clipboard",
            "",
        ));
        out.push(PaletteEntry::command(
            PaletteAction::ExportTranscript,
            "document-save-symbolic",
            "Export conversation to file…",
            "Save the transcript as a .md file",
            "",
        ));
    }
    if connected && state.selected.is_some() {
        let pinned = state.is_selected_pinned();
        let mut pin = PaletteEntry::command(
            PaletteAction::TogglePinCurrent,
            if pinned {
                "non-starred-symbolic"
            } else {
                "starred-symbolic"
            },
            if pinned {
                "Unpin current conversation"
            } else {
                "Pin current conversation"
            },
            "Keep important threads anchored at the top of the sidebar (/pin)",
            "Ctrl+P",
        );
        if pinned {
            pin.badge = "PINNED".into();
        }
        out.push(pin);
        out.push(PaletteEntry::command(
            PaletteAction::RenameCurrent,
            "document-edit-symbolic",
            "Rename current conversation…",
            "Set a custom title persisted on the Hermes server (/rename)",
            "F2",
        ));
        out.push(PaletteEntry::command(
            PaletteAction::ArchiveCurrent,
            "folder-download-symbolic",
            "Archive current conversation",
            "Hide from active lists while keeping server history searchable (/archive)",
            "Ctrl+Shift+E",
        ));
    }
    out
}

fn palette_timeline_commands(
    state: &AppState,
    conversation: Option<&Conversation>,
    connected: bool,
    writable: bool,
) -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    if connected
        && !state.creating
        && conversation
            .is_some_and(|c| !c.status.busy() && count_visible_branch_messages(&c.messages) > 0)
    {
        out.push(PaletteEntry::command(
            PaletteAction::BranchCurrent,
            "media-playlist-shuffle-symbolic",
            "Branch conversation",
            "Fork saved history into an independent writable thread",
            "Ctrl+D",
        ));
    }
    if let Some((parent_id, parent_title)) = state
        .selected
        .as_deref()
        .and_then(|id| state.parent_for(id))
    {
        let mut entry = PaletteEntry::command(
            PaletteAction::OpenSession(parent_id),
            "go-up-symbolic",
            format!("Open parent conversation: {parent_title}"),
            "Jump back to the conversation this branch forked from",
            "",
        );
        entry.badge = "PARENT".into();
        out.push(entry);
    }
    if writable
        && conversation.is_some_and(|c| {
            !c.status.busy() && !c.rewinding && last_turn_targets(&c.messages).0.is_some()
        })
    {
        out.push(PaletteEntry::command(
            PaletteAction::RewindLast(RewindMode::EditRetry),
            "document-edit-symbolic",
            "Edit & retry last turn",
            "Rewind the last exchange and load your prompt into the composer",
            "",
        ));
        out.push(PaletteEntry::command(
            PaletteAction::RewindLast(RewindMode::Regenerate),
            "view-refresh-symbolic",
            "Regenerate last response",
            "Rewind the last exchange and run the prompt again",
            "",
        ));
    }
    out
}

fn palette_inspector_commands(state: &AppState, writable: bool) -> Vec<PaletteEntry> {
    let mut out = Vec::new();
    if writable {
        out.push(PaletteEntry::command(
            PaletteAction::OpenInspector(InspectorTab::SideAsk),
            "dialog-question-symbolic",
            "Ask a side question (/btw)…",
            "Query a snapshot of this chat without polluting history",
            "Alt+Enter",
        ));
    }
    let mut toggle = PaletteEntry::command(
        PaletteAction::ToggleInspector,
        "utilities-system-monitor-symbolic",
        "Toggle Flight Deck inspector",
        "Show or hide side-ask, live trace, and conversation map",
        "Ctrl+I",
    );
    toggle.active = state.inspector_open;
    if toggle.active {
        toggle.badge = "OPEN".into();
    }
    out.push(toggle);
    if state.selected.is_some() {
        out.push(PaletteEntry::command(
            PaletteAction::OpenInspector(InspectorTab::Trace),
            "utilities-terminal-symbolic",
            "Inspect live reasoning & tool trace",
            "View model chain-of-thought, tool timings, and context gauge",
            "",
        ));
        out.push(PaletteEntry::command(
            PaletteAction::OpenInspector(InspectorTab::Outline),
            "view-list-bullet-symbolic",
            "Jump to turn in conversation outline",
            "Navigate prompts and code blocks non-linearly",
            "",
        ));
    }
    out
}

fn palette_filter_commands(state: &AppState) -> Vec<PaletteEntry> {
    let (all, chats, auto) = state.filter_counts();
    let pinned = state.pinned_count();
    let specs = [
        (
            SessionFilter::Pinned,
            "starred-symbolic",
            "Show pinned conversations",
            format!("{pinned} starred threads"),
        ),
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
            "Filter & search all conversations",
            "Focus sidebar search (queries local + full server FTS5)",
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

fn palette_sessions(state: &AppState, query: &str, now_epoch_secs: i64) -> Vec<PaletteEntry> {
    let fts_active = state.has_active_remote_search(query);
    let mut ordered: Vec<&SessionSummary> = state.sessions.iter().collect();
    ordered.sort_by_key(|s| !s.pinned);
    ordered
        .into_iter()
        .map(|session| {
            build_local_palette_session(state, session, query, fts_active, now_epoch_secs)
        })
        .collect()
}

fn build_local_palette_session(
    state: &AppState,
    session: &SessionSummary,
    query: &str,
    fts_active: bool,
    now_epoch_secs: i64,
) -> PaletteEntry {
    let icon = session_palette_icon(&session.source, session.pinned);
    let title = fallback_session_title(&session.title);
    let fts_hit = fts_active
        .then(|| {
            state
                .remote_search_hits
                .iter()
                .find(|h| h.stored_id == session.stored_id)
        })
        .flatten();
    let (action, subtitle, subtitle_markup) = if let Some(hit) = fts_hit {
        (
            PaletteAction::OpenSearchHit {
                stored_id: session.stored_id.clone(),
                query: query.trim().to_string(),
            },
            hit.snippet_plain.clone(),
            Some(hit.snippet_markup.clone()),
        )
    } else {
        let mut sub = session_meta_line(session, now_epoch_secs);
        if session.is_branch {
            sub.push_str(" · fork");
        }
        (
            PaletteAction::OpenSession(session.stored_id.clone()),
            sub,
            None,
        )
    };
    PaletteEntry {
        action,
        icon,
        title,
        subtitle,
        subtitle_markup,
        shortcut: "",
        badge: if session.pinned {
            "★ PIN".into()
        } else {
            source_badge(&session.source)
        },
        active: state.selected.as_deref() == Some(session.stored_id.as_str()),
        destructive: false,
    }
}

fn palette_remote_hits(state: &AppState, query: &str, now_epoch_secs: i64) -> Vec<PaletteEntry> {
    if !state.has_active_remote_search(query) {
        return Vec::new();
    }
    let known: std::collections::HashSet<&str> = state
        .sessions
        .iter()
        .map(|s| s.stored_id.as_str())
        .collect();
    let trimmed = query.trim().to_string();
    state
        .remote_search_hits
        .iter()
        .filter(|hit| !known.contains(hit.stored_id.as_str()))
        .map(|hit| build_remote_palette_entry(state, hit, &trimmed, now_epoch_secs))
        .collect()
}

fn build_remote_palette_entry(
    state: &AppState,
    hit: &SearchHit,
    query: &str,
    now_epoch_secs: i64,
) -> PaletteEntry {
    let age = format_relative_age(&hit.updated_at, now_epoch_secs).unwrap_or_default();
    let subtitle = if hit.snippet_plain.is_empty() {
        format!("{} msgs · {age}", hit.message_count)
    } else {
        hit.snippet_plain.clone()
    };
    let subtitle_markup = (!hit.snippet_markup.is_empty()).then(|| hit.snippet_markup.clone());
    PaletteEntry {
        action: PaletteAction::OpenSearchHit {
            stored_id: hit.stored_id.clone(),
            query: query.to_string(),
        },
        icon: session_palette_icon(&hit.source, hit.pinned),
        title: fallback_session_title(&hit.title),
        subtitle,
        subtitle_markup,
        shortcut: "",
        badge: "FTS5".into(),
        active: state.selected.as_deref() == Some(hit.stored_id.as_str()),
        destructive: false,
    }
}

fn session_palette_icon(source: &str, pinned: bool) -> &'static str {
    if pinned {
        "starred-symbolic"
    } else if classify_source(source) == SourceKind::Automated {
        "alarm-symbolic"
    } else {
        "mail-send-symbolic"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcript_export_renders_roles_and_safe_names() {
        let msgs = vec![
            ChatMessage {
                id: "1".into(),
                role: Role::User,
                text: "hi".into(),
            },
            ChatMessage {
                id: "2".into(),
                role: Role::Assistant,
                text: "yo\n".into(),
            },
        ];
        let md = transcript_markdown(" Title ", &msgs);
        assert_eq!(md, "# Title\n\n## You\n\nhi\n\n## Hermes\n\nyo\n");
        assert_eq!(export_file_name("Fix: the /bug!"), "fix-the-bug.md");
        assert_eq!(export_file_name("???"), "conversation.md");
    }

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
            ..Default::default()
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

        let btw = build_palette_entries(&state, "btw", 1_700_000_600);
        assert_eq!(
            btw[0].action,
            PaletteAction::OpenInspector(InspectorTab::SideAsk)
        );
    }

    #[test]
    fn composer_input_distinguishes_btw_side_questions_from_normal_prompts() {
        assert_eq!(parse_composer_input("   \n "), ComposerIntent::Empty);
        assert_eq!(parse_composer_input("/btw"), ComposerIntent::Empty);
        assert_eq!(parse_composer_input("/ask   "), ComposerIntent::Empty);
        assert_eq!(
            parse_composer_input("/btw what port did we pick?"),
            ComposerIntent::SideQuestion("what port did we pick?".into())
        );
        assert_eq!(
            parse_composer_input("  /ask\nexplain this stack frame  "),
            ComposerIntent::SideQuestion("explain this stack frame".into())
        );
        assert_eq!(
            parse_composer_input("/btwitter is not a prefix"),
            ComposerIntent::Prompt("/btwitter is not a prefix".into())
        );
        assert_eq!(
            parse_composer_input("normal prompt"),
            ComposerIntent::Prompt("normal prompt".into())
        );
    }

    #[test]
    fn telemetry_parses_v023_usage_and_live_session_info_cleanly() {
        let usage = serde_json::json!({
            "usage": {
                "input": 14_200,
                "output": 850,
                "reasoning": 320,
                "cache_read": 11_000,
                "total": 15_050,
                "calls": 3,
                "compressions": 1,
                "context_used": 14_500,
                "context_max": 128_000,
                "avg_tps": 64.5,
                "avg_latency_s": 1.25,
                "cost_usd": 0.042
            }
        });
        let parsed = parse_telemetry_payload(&usage).expect("telemetry");
        assert_eq!(parsed.input_tokens, 14_200);
        assert_eq!(parsed.output_tokens, 850);
        assert_eq!(parsed.reasoning_tokens, 320);
        assert_eq!(parsed.cache_read_tokens, 11_000);
        assert_eq!(parsed.context_percent, Some(11));
        assert_eq!(parsed.header_badge().as_deref(), Some("11% ctx"));
        assert_eq!(parsed.context_summary(), "14.5k / 128.0k (11%)");
        assert!(parsed.context_fraction().unwrap() > 0.10);

        let resume = serde_json::json!({
            "info": {
                "cwd": "/srv/hermes/workspace",
                "branch": "feat/flight-deck",
                "usage": { "input": 500, "output": 100 }
            }
        });
        let mut live = parse_live_info_telemetry(&resume).expect("live info");
        assert_eq!(live.cwd, "/srv/hermes/workspace");
        assert_eq!(live.branch, "feat/flight-deck");
        assert_eq!(live.header_badge().as_deref(), Some("600 tok"));
        live.merge(parsed);
        assert_eq!(live.cwd, "/srv/hermes/workspace");
        assert_eq!(live.context_percent, Some(11));
        assert_eq!(format_compact_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn side_question_lifecycle_handles_fast_completion_errors_and_disconnect() {
        let mut state = AppState::default();
        let id1 = state.allocate_side_id();
        let id2 = state.allocate_side_id();
        let id3 = state.allocate_side_id();
        let mut chat = Conversation {
            runtime_id: Some("rt-1".into()),
            status: TurnStatus::Streaming,
            ..Default::default()
        };
        let rev0 = chat.revision;
        chat.begin_side_question(id1, "What word?".into());
        assert_eq!(chat.revision, rev0, "side channel never diffs transcript");
        // Fast completion arriving before SideStarted binds task_id:
        chat.complete_side_question("btw_fast", "What word?", "The word is **lattice**.".into());
        assert_eq!(chat.side_questions[0].status, SideQuestionStatus::Answered);
        assert_eq!(chat.side_questions[0].task_id.as_deref(), Some("btw_fast"));

        // Normal bind + gateway side-agent error prefix:
        chat.begin_side_question(id2, "Trigger side error".into());
        chat.bind_side_task(id2, "btw_err".into());
        chat.complete_side_question("btw_err", "Trigger side error", "error: rate limit".into());
        assert_eq!(chat.side_questions[1].status, SideQuestionStatus::Failed);

        // Pending question settled cleanly on disconnect while main turn stays independent:
        chat.begin_side_question(id3, "Still running".into());
        state.conversations.insert("s1".into(), chat);
        state.disconnect();
        let settled = &state.conversations["s1"];
        assert_eq!(settled.side_questions[2].status, SideQuestionStatus::Failed);
        assert!(settled.side_questions[2].answer.contains("Connection lost"));
    }

    #[test]
    fn reasoning_tool_trace_and_outline_track_conversation_milestones() {
        let mut chat = Conversation::default();
        chat.append_reasoning("Step 1. ", false);
        chat.append_reasoning("Step 2.", false);
        assert_eq!(chat.reasoning, "Step 1. Step 2.");
        chat.append_reasoning("Replaced block", true);
        assert_eq!(chat.reasoning, "Replaced block");
        chat.clear_turn_reasoning();
        assert!(chat.reasoning.is_empty());

        chat.apply_tool_event(ToolEventUpdate::Started {
            tool_id: "call-1".into(),
            name: "terminal".into(),
            detail: "cargo test".into(),
        });
        chat.apply_tool_event(ToolEventUpdate::Progress {
            tool_id: "call-1".into(),
            name: "terminal".into(),
            percent: Some(50),
            message: "compiling".into(),
        });
        chat.apply_tool_event(ToolEventUpdate::Risk {
            tool_id: "call-1".into(),
            name: "terminal".into(),
            risk: "high entropy output".into(),
        });
        chat.apply_tool_event(ToolEventUpdate::Completed {
            tool_id: "call-1".into(),
            name: "terminal".into(),
            duration_s: Some(1.4),
            summary: "ok (53 passed)".into(),
        });
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(
            chat.tools[0].state,
            ToolState::RiskFlagged("high entropy output".into())
        );
        assert_eq!(chat.tools[0].duration_s, Some(1.4));

        chat.record_message(ChatMessage {
            id: "u1".into(),
            role: Role::User,
            text: "Write a Rust snippet".into(),
        });
        chat.record_message(ChatMessage {
            id: "t1".into(),
            role: Role::Tool,
            text: "Used terminal".into(),
        });
        chat.record_message(ChatMessage {
            id: "a1".into(),
            role: Role::Assistant,
            text: "Here:\n```rust\nfn main() {}\n```\nDone.".into(),
        });
        let outline = build_outline(&chat.messages);
        assert_eq!(outline.len(), 1);
        assert_eq!(outline[0].turn_number, 1);
        assert_eq!(outline[0].message_index, 0);
        assert_eq!(outline[0].end_message_index, 2);
        assert_eq!(outline[0].code_blocks, 1);
        assert_eq!(outline[0].meta, "1 tool · 1 code block");
    }

    #[test]
    fn slash_commands_parse_branch_fork_undo_retry_and_regenerate() {
        assert_eq!(
            parse_composer_input("/branch"),
            ComposerIntent::Branch(None)
        );
        assert_eq!(
            parse_composer_input("  /branch    Try async channel  "),
            ComposerIntent::Branch(Some("Try async channel".into()))
        );
        assert_eq!(
            parse_composer_input("/fork Wayland experiment"),
            ComposerIntent::Branch(Some("Wayland experiment".into()))
        );
        assert_eq!(
            parse_composer_input("/undo"),
            ComposerIntent::Rewind(RewindMode::Undo)
        );
        assert_eq!(
            parse_composer_input("/retry"),
            ComposerIntent::Rewind(RewindMode::EditRetry)
        );
        assert_eq!(
            parse_composer_input("/regen"),
            ComposerIntent::Rewind(RewindMode::Regenerate)
        );
        assert_eq!(
            parse_composer_input("/regenerate"),
            ComposerIntent::Rewind(RewindMode::Regenerate)
        );
        assert_eq!(RewindMode::Undo.wire_intent(), "undo");
        assert_eq!(RewindMode::EditRetry.wire_intent(), "retry");
        assert_eq!(RewindMode::Regenerate.wire_intent(), "retry");
    }

    #[test]
    fn visible_branch_counts_and_seeds_skip_tool_rows_and_detect_prefix() {
        let messages = vec![
            ChatMessage {
                id: "u1".into(),
                role: Role::User,
                text: "First prompt".into(),
            },
            ChatMessage {
                id: "t1".into(),
                role: Role::Tool,
                text: "Used grep".into(),
            },
            ChatMessage {
                id: "a1".into(),
                role: Role::Assistant,
                text: "First answer".into(),
            },
            ChatMessage {
                id: "u2".into(),
                role: Role::User,
                text: "Second prompt".into(),
            },
            ChatMessage {
                id: "a2".into(),
                role: Role::Assistant,
                text: "Second answer".into(),
            },
        ];
        assert_eq!(count_visible_branch_messages(&messages), 4);
        assert_eq!(visible_branch_count_through(&messages, 0), Some(1));
        assert_eq!(visible_branch_count_through(&messages, 2), Some(2));
        assert_eq!(visible_branch_count_through(&messages, 4), None);

        let seeded = seed_messages_through(&messages, Some(2));
        assert_eq!(
            seeded,
            vec![
                SeedMessage {
                    role: "user".into(),
                    content: "First prompt".into()
                },
                SeedMessage {
                    role: "assistant".into(),
                    content: "First answer".into()
                },
            ]
        );
        assert_eq!(
            last_turn_targets(&messages),
            (
                Some("u2".into()),
                Some("a2".into()),
                Some("Second prompt".into())
            )
        );
    }

    #[test]
    fn lineage_sync_flags_true_forks_without_marking_compressed_predecessors() {
        let mut state = AppState {
            sessions: vec![
                SessionSummary {
                    stored_id: "root".into(),
                    title: "Architecture design".into(),
                    source: "tui".into(),
                    ..Default::default()
                },
                SessionSummary {
                    stored_id: "fork-1".into(),
                    title: "Architecture design (branch)".into(),
                    source: "tui".into(),
                    parent_session_id: Some("root".into()),
                    ..Default::default()
                },
                SessionSummary {
                    stored_id: "compressed-tip".into(),
                    title: "Long running chat".into(),
                    source: "tui".into(),
                    parent_session_id: Some("hidden-old-segment".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        state.sync_branch_flags();
        assert!(!state.sessions[0].is_branch);
        assert!(state.sessions[1].is_branch);
        assert!(!state.sessions[2].is_branch);
        assert_eq!(
            state.parent_for("fork-1"),
            Some(("root".into(), "Architecture design".into()))
        );
        assert_eq!(state.parent_for("compressed-tip"), None);
        assert_eq!(state.branches_of("root").len(), 1);

        state.session_query = "fork".into();
        assert_eq!(state.visible_sessions(), vec![1]);
    }

    #[test]
    fn palette_surfaces_branch_rewind_and_parent_jump_actions() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            selected: Some("fork-1".into()),
            sessions: vec![
                SessionSummary {
                    stored_id: "root".into(),
                    title: "Base chat".into(),
                    source: "tui".into(),
                    ..Default::default()
                },
                SessionSummary {
                    stored_id: "fork-1".into(),
                    title: "Base chat (branch)".into(),
                    source: "tui".into(),
                    parent_session_id: Some("root".into()),
                    is_branch: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        state.conversations.insert(
            "fork-1".into(),
            Conversation {
                runtime_id: Some("rt-fork".into()),
                messages: vec![
                    ChatMessage {
                        id: "u1".into(),
                        role: Role::User,
                        text: "Hello".into(),
                    },
                    ChatMessage {
                        id: "a1".into(),
                        role: Role::Assistant,
                        text: "Hi".into(),
                    },
                ],
                ..Default::default()
            },
        );
        let entries = build_palette_entries(&state, "", 1_700_000_000);
        assert!(
            entries
                .iter()
                .any(|e| e.action == PaletteAction::BranchCurrent)
        );
        assert!(
            entries
                .iter()
                .any(|e| e.action == PaletteAction::RewindLast(RewindMode::EditRetry))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.action == PaletteAction::RewindLast(RewindMode::Regenerate))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.badge == "PARENT"
                    && e.action == PaletteAction::OpenSession("root".into()))
        );
    }

    #[test]
    fn curation_slash_commands_parse_cleanly() {
        assert_eq!(
            parse_composer_input("/find"),
            ComposerIntent::FindInTranscript(None)
        );
        assert_eq!(
            parse_composer_input("/search tokio channel"),
            ComposerIntent::FindInTranscript(Some("tokio channel".into()))
        );
        assert_eq!(parse_composer_input("/pin"), ComposerIntent::TogglePin);
        assert_eq!(parse_composer_input("/archive"), ComposerIntent::Archive);
        assert_eq!(parse_composer_input("/rename"), ComposerIntent::Empty);
        assert_eq!(
            parse_composer_input("/rename Hyprland monitor layout"),
            ComposerIntent::Rename("Hyprland monitor layout".into())
        );
    }

    #[test]
    fn pinned_sessions_float_first_and_mutations_sync_state() {
        let mut state = AppState {
            selected: Some("s2".into()),
            sessions: vec![
                sample_session("s1", "First unpinned", "tui", 4),
                SessionSummary {
                    pinned: true,
                    model: "anthropic/claude-sonnet-4-5".into(),
                    ..sample_session("s2", "Starred thread", "tui", 9)
                },
                sample_session("s3", "Third unpinned", "cron", 2),
            ],
            ..Default::default()
        };
        assert_eq!(state.pinned_count(), 1);
        assert!(state.is_selected_pinned());
        assert_eq!(state.visible_sessions(), vec![1, 0, 2]);
        state.session_filter = SessionFilter::Pinned;
        assert_eq!(state.visible_sessions(), vec![1]);
        assert!(session_meta_line(&state.sessions[1], 1_700_000_120).contains("claude-sonnet-4-5"));

        assert_eq!(
            state.apply_mutation("s2", &SessionMutation::Rename("Renamed star".into())),
            MutationOutcome::Updated
        );
        assert_eq!(state.sessions[1].title, "Renamed star");
        assert_eq!(
            state.apply_mutation("s2", &SessionMutation::SetPinned(false)),
            MutationOutcome::Updated
        );
        assert!(!state.is_selected_pinned());
        assert_eq!(
            state.apply_mutation("s2", &SessionMutation::Archive),
            MutationOutcome::ArchivedSelected
        );
        assert!(state.selected.is_none());
        assert_eq!(state.sessions.len(), 2);
    }

    #[test]
    fn transcript_find_steps_and_tracks_active_match() {
        let msgs = vec![
            ChatMessage {
                id: "1".into(),
                role: Role::User,
                text: "Check tailscale funnel".into(),
            },
            ChatMessage {
                id: "2".into(),
                role: Role::Assistant,
                text: "Done configuring".into(),
            },
            ChatMessage {
                id: "3".into(),
                role: Role::User,
                text: "Verify tailscale status".into(),
            },
        ];
        let mut find = TranscriptFind::default();
        find.open_with("tailscale", &msgs);
        assert_eq!(find.matches, vec![0, 2]);
        assert_eq!(find.status_label(), "1 / 2");
        assert_eq!(find.match_state(0), (true, true));
        assert_eq!(find.match_state(2), (true, false));
        assert_eq!(find.step(1), Some(2));
        assert_eq!(find.status_label(), "2 / 2");
        assert_eq!(find.step(1), Some(0));
        assert_eq!(find.step(-1), Some(2));
        find.close();
        assert_eq!(find.match_state(0), (false, false));
    }

    #[test]
    fn palette_and_sidebar_merge_remote_fts5_hits_without_duplicates() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            sessions: vec![sample_session("local-1", "Daily notes", "tui", 6)],
            session_query: "lattice".into(),
            ..Default::default()
        };
        let (seq, q) = state.begin_remote_search("lattice").expect("search seq");
        assert_eq!(q, "lattice");
        assert!(!state.apply_search_results("lattice".into(), seq + 9, Vec::new()));
        let hits = vec![
            SearchHit {
                stored_id: "local-1".into(),
                title: "Daily notes".into(),
                source: "tui".into(),
                snippet_plain: "Reply with lattice".into(),
                snippet_markup: "Reply with <b>lattice</b>".into(),
                ..Default::default()
            },
            SearchHit {
                stored_id: "deep-999".into(),
                title: "Archived months ago".into(),
                source: "local".into(),
                snippet_plain: "Found lattice crystal".into(),
                snippet_markup: "Found <b>lattice</b> crystal".into(),
                message_count: 18,
                updated_at: "1700000000".into(),
                ..Default::default()
            },
        ];
        assert!(state.apply_search_results("lattice".into(), seq, hits));
        assert_eq!(state.remote_only_sidebar_hits().len(), 1);
        let entries = build_palette_entries(&state, "lattice", 1_700_000_300);
        assert!(entries.iter().any(|e| {
            e.action
                == PaletteAction::OpenSearchHit {
                    stored_id: "local-1".into(),
                    query: "lattice".into(),
                }
        }));
        assert!(entries.iter().any(|e| {
            e.badge == "FTS5"
                && e.action
                    == PaletteAction::OpenSearchHit {
                        stored_id: "deep-999".into(),
                        query: "lattice".into(),
                    }
        }));
    }

    #[test]
    fn studio_commands_attachments_and_compaction_parse_and_stage() {
        assert_eq!(parse_composer_input("/compress"), ComposerIntent::Compress);
        assert_eq!(parse_composer_input("/compact"), ComposerIntent::Compress);
        assert_eq!(parse_composer_input("/attach"), ComposerIntent::AttachImage);
        assert_eq!(
            parse_composer_input("/workspace hermes-native"),
            ComposerIntent::PickWorkspace(Some("hermes-native".into()))
        );
        assert_eq!(
            parse_composer_input("/workspace"),
            ComposerIntent::PickWorkspace(None)
        );

        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            launch_cwd: Some("/home/alice/repo".into()),
            ..Default::default()
        };
        assert_eq!(state.effective_active_cwd(), Some("/home/alice/repo"));
        let png = vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
        ];
        let id1 = state.allocate_attachment_id();
        let img = StagedImage::from_bytes(id1, "screen.png", png).unwrap();
        assert_eq!(state.stage_active_images(vec![img]).unwrap(), 1);
        assert_eq!(state.active_attachments().len(), 1);
        let (title, prompt) = state.begin_launch_submit().expect("image-only launch");
        assert_eq!(title, "Image: screen.png");
        assert_eq!(prompt, "");
        state.cancel_pending_launch();
        assert_eq!(state.active_attachments().len(), 1);
        let _ = state.begin_launch_submit().unwrap();
        let (_, bundled) = state.take_pending_launch_bundle().unwrap();
        assert_eq!(bundled.len(), 1);
        assert!(state.active_attachments().is_empty());
    }
}
