use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::error::TrySendError;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use crate::model::{
    Catalog, ChatMessage, ClarificationQuestion, Command, CompressSummary, Config, ConnectionState,
    Event, InteractionAnswer, InteractionKind, InteractionRequest, ModelIdentity, RewindMode, Role,
    SeedMessage, SessionMutation, SessionSummary, SkillSummary, StagedImage, SteerOutcome,
    ToolEventUpdate, TurnStatus, WorkspaceCatalog, parse_live_info_telemetry,
    parse_telemetry_payload, seed_messages_through,
};
use crate::recall;
use crate::studio;

const PAGE: usize = 50;
const SEARCH_LIMIT: usize = 25;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const COMPRESS_TIMEOUT: Duration = Duration::from_secs(90);
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const PROBE_SILENCE: Duration = Duration::from_secs(30);
/// Bound for socket writes. A write that cannot complete in this window means
/// the gateway stopped reading; dropping the link lets the automatic
/// reconnect take over instead of stalling the transport loop for a full
/// RPC response wait.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Inbound silence bound. Long enough to cover a server-side compress
/// (`COMPRESS_TIMEOUT`) that pushes no intermediate frames; anything quieter
/// despite keepalive probes is a dead link.
const SILENCE_BOUND: Duration = Duration::from_secs(90);
/// Quiet window after which heartbeat mode sends a `gateway.ping`.
const HEARTBEAT_SILENCE: Duration = Duration::from_secs(20);
/// Per-runtime cap on delta text buffered while the UI is too slow to keep up.
/// `MessageComplete` carries the authoritative text, so dropping the front of a
/// capped tail degrades only live streaming display, never the transcript.
const DELTA_TAIL_CAP: usize = 1 << 20;
const EVENT_BOUND: usize = 512;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Backend {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: std::sync::Mutex<tokio::sync::mpsc::Receiver<Event>>,
}

pub fn spawn(config: Config) -> Backend {
    let (commands, incoming) = tokio::sync::mpsc::unbounded_channel();
    let (events_tx, events_rx) = tokio::sync::mpsc::channel(EVENT_BOUND);
    let failure_events = events_tx.clone();
    let worker = std::thread::Builder::new()
        .name("hermes-net".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    let _ =
                        events_tx.blocking_send(Event::Connection(ConnectionState::Disconnected(
                            "Could not start the network runtime.".into(),
                        )));
                    return;
                }
            };
            runtime.block_on(run(config, incoming, events_tx));
        });
    if worker.is_err() {
        let _ = failure_events.try_send(Event::Connection(ConnectionState::Disconnected(
            "Could not start the connection worker.".into(),
        )));
    }
    Backend {
        commands,
        events: std::sync::Mutex::new(events_rx),
    }
}

impl Backend {
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "The connection worker has stopped.".to_string())
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

struct Ask {
    method: String,
    params: Value,
    runtime_id: String,
    rpc_id: Value,
}

struct Turn {
    message_id: Arc<str>,
    segment: i64,
    applied_offset: i64,
    generation: u64,
    phase: TurnPhase,
    submit_settled: bool,
}

/// One parked streaming tail. Ids are shared `Arc<str>`s so a steady delta
/// frame clones refcounts instead of allocating strings; the entry survives
/// successful sends (with an empty tail) so a keeping-up UI costs one
/// allocation per frame — the delta buffer itself.
struct PendingDelta {
    runtime: Arc<str>,
    message: Arc<str>,
    tail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnPhase {
    Submitting,
    Streaming,
    AwaitingInput,
    /// Interrupt accepted; generation stays live until message.complete or error.
    Stopping,
    Terminal,
}

impl Turn {
    fn submitting(message_id: Arc<str>, generation: u64) -> Self {
        Self {
            message_id,
            generation,
            phase: TurnPhase::Submitting,
            submit_settled: false,
            segment: 0,
            applied_offset: 0,
        }
    }

    fn streaming(message_id: Arc<str>, generation: u64) -> Self {
        Self {
            message_id,
            generation,
            phase: TurnPhase::Streaming,
            submit_settled: true,
            segment: 0,
            applied_offset: 0,
        }
    }

    fn acknowledge(&mut self, generation: u64) -> bool {
        if self.generation != generation
            || self.phase != TurnPhase::Submitting
            || self.submit_settled
        {
            return false;
        }
        self.phase = TurnPhase::Streaming;
        self.submit_settled = true;
        true
    }

    fn fail_submit(&mut self, generation: u64) -> bool {
        if self.generation != generation
            || self.phase != TurnPhase::Submitting
            || self.submit_settled
        {
            return false;
        }
        self.submit_settled = true;
        true
    }

    fn accept_interrupt(&mut self, generation: u64) -> bool {
        if self.generation != generation
            || matches!(self.phase, TurnPhase::Terminal | TurnPhase::Stopping)
        {
            return false;
        }
        self.phase = TurnPhase::Stopping;
        self.submit_settled = true;
        true
    }

    fn fail_interrupt(&self, generation: u64) -> bool {
        self.generation == generation
            && !matches!(self.phase, TurnPhase::Terminal | TurnPhase::Stopping)
    }

    fn note_accepted(&mut self) -> bool {
        if self.phase == TurnPhase::Terminal {
            return false;
        }
        self.submit_settled = true;
        if self.phase == TurnPhase::Submitting {
            self.phase = TurnPhase::Streaming;
        }
        true
    }

    fn await_input(&mut self) {
        if matches!(self.phase, TurnPhase::Terminal | TurnPhase::Stopping) {
            return;
        }
        self.submit_settled = true;
        self.phase = TurnPhase::AwaitingInput;
    }

    fn note_withdraw(&mut self) {
        if self.phase == TurnPhase::AwaitingInput {
            self.phase = TurnPhase::Streaming;
        }
    }

    fn finish(&mut self) -> bool {
        if self.phase == TurnPhase::Terminal {
            return false;
        }
        self.phase = TurnPhase::Terminal;
        self.submit_settled = true;
        true
    }
}

/// Where a busy Enter goes. Matches upstream canonical `interrupt` policy
/// without the hard-interrupt fallback: `session.redirect` first, a queue-only
/// submit on `rejected`, a plain submit only when no live turn is tracked.
enum SteerRoute {
    Drop,
    Redirect,
    Submit,
}

fn steer_route(turn: Option<&Turn>) -> SteerRoute {
    match turn.map(|turn| turn.phase) {
        Some(TurnPhase::Stopping) => SteerRoute::Drop,
        Some(TurnPhase::Submitting | TurnPhase::Streaming | TurnPhase::AwaitingInput) => {
            SteerRoute::Redirect
        }
        Some(TurnPhase::Terminal) | None => SteerRoute::Submit,
    }
}

/// The `rejected` fallback shape: queue-only, never hard-interrupts.
fn queued_submit_params(runtime_id: &str, text: &str) -> Value {
    json!({"session_id": runtime_id, "text": text, "queued": true})
}

fn default_branch_title(parent_title: &str, custom_title: Option<&str>) -> String {
    if let Some(name) = custom_title.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let clean = parent_title.trim();
    if clean.is_empty() {
        "Conversation (branch)".into()
    } else {
        format!("{clean} (branch)")
    }
}

fn branch_rpc_params(runtime_id: &str, custom_title: Option<&str>, count: Option<usize>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("session_id".into(), Value::String(runtime_id.to_string()));
    if let Some(name) = custom_title.map(str::trim).filter(|s| !s.is_empty()) {
        map.insert("name".into(), Value::String(name.to_string()));
    }
    if let Some(limit) = count.filter(|&n| n > 0) {
        map.insert("count".into(), json!(limit));
    }
    Value::Object(map)
}

fn seeded_branch_params(title: &str, parent_stored_id: &str, seed: &[SeedMessage]) -> Value {
    let rows: Vec<Value> = seed
        .iter()
        .map(|m| json!({"role": m.role, "content": m.content}))
        .collect();
    json!({
        "title": title,
        "parent_session_id": parent_stored_id,
        "messages": rows,
    })
}

fn undo_rpc_params(runtime_id: &str, mode: RewindMode) -> Value {
    json!({
        "session_id": runtime_id,
        "intent": mode.wire_intent(),
    })
}

fn create_rpc_params(title: &str, cwd: Option<&str>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("title".into(), Value::String(title.to_string()));
    if let Some(dir) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        map.insert("cwd".into(), Value::String(dir.to_string()));
    }
    Value::Object(map)
}

fn extract_session_ids(result: &Value) -> Option<(String, String)> {
    let runtime_id = result
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let stored_id = result
        .get("stored_session_id")
        .or_else(|| result.get("session_key"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some((runtime_id, stored_id))
}

struct PendingBranch {
    stored_id: String,
    temp_runtime_id: String,
    parent_stored_id: String,
    parent_title: String,
    title: String,
    count: usize,
    fallback_messages: Vec<ChatMessage>,
    model: Option<ModelIdentity>,
}

struct Job {
    kind: JobKind,
    deadline: Instant,
}

enum JobKind {
    Capabilities,
    Ping,
    Create {
        title: String,
    },
    Resume {
        stored_id: String,
    },
    Submit {
        runtime_id: String,
        generation: u64,
    },
    AttachImage {
        runtime_id: String,
        generation: u64,
        remaining_paths: VecDeque<String>,
        prompt_text: String,
    },
    Interrupt {
        runtime_id: String,
        generation: u64,
    },
    /// Busy-turn steer over `session.redirect`. Carries the text for the
    /// `rejected` fallback, which re-submits it queue-only.
    Steer {
        runtime_id: String,
        text: String,
    },
    /// Queue-only `prompt.submit` (`queued: true`). Never hard-interrupts.
    Queue {
        runtime_id: String,
    },
    Close {
        stored_id: String,
        retried: bool,
    },
    Delete {
        stored_id: String,
        retried: bool,
    },
    DeleteResume {
        stored_id: String,
    },
    ArchiveClose {
        stored_id: String,
        retried: bool,
    },
    ArchiveResume {
        stored_id: String,
    },
    Catalog,
    /// Provider-less prime on a chat whose agent is not built yet.
    Prime {
        runtime_id: String,
        target: ModelIdentity,
        confirm_expensive: bool,
    },
    Switch {
        runtime_id: String,
        target: ModelIdentity,
    },
    SideQuestion {
        runtime_id: String,
        client_id: u64,
    },
    Branch {
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_seed: Vec<SeedMessage>,
    },
    BranchClose(PendingBranch),
    BranchResume(PendingBranch),
    BranchStored {
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_seed: Vec<SeedMessage>,
    },
    BranchSeeded {
        parent_stored_id: String,
        parent_title: String,
        title: String,
        seed_count: usize,
    },
    Undo {
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
        attempts: u8,
    },
    UndoSync {
        stored_id: String,
        runtime_id: String,
        removed: usize,
        mode: RewindMode,
        prompt_text: String,
    },
    Compress {
        stored_id: String,
        runtime_id: String,
    },
    CompressSync {
        stored_id: String,
        runtime_id: String,
        summary: CompressSummary,
    },
    SessionInfo {
        runtime_id: String,
    },
}

struct Link {
    http: reqwest::Client,
    events: tokio::sync::mpsc::Sender<Event>,
    origin: url::Url,
    username: String,
    password: String,
    secrets: Vec<String>,
    read: Option<futures_util::stream::SplitStream<Socket>>,
    write: Option<futures_util::stream::SplitSink<Socket, Message>>,
    next_id: u64,
    next_turn_generation: u64,
    /// Connection epoch: bumped on every socket teardown, so off-loop work
    /// results stamped with an older epoch are discarded instead of applied
    /// to a connection they were not fetched for.
    epoch: u64,
    message_counts: HashMap<String, usize>,
    pinned_sessions: HashSet<String>,
    deferred_mutations: HashMap<String, (Option<String>, Option<bool>)>,
    /// Sessions with a patch in flight (user-initiated or deferred flush).
    /// A second concurrent patch would race the fenced revision and surface
    /// HTTP 409 to the user, so colliding mutations ride the deferral
    /// instead. Cleared on teardown: epoch-guarded results from a torn-down
    /// connection never reach the apply paths that clear it.
    patching: HashSet<String>,
    jobs: HashMap<u64, Job>,
    runtime_of: HashMap<String, String>,
    refused: HashSet<String>,
    asks: HashMap<String, Ask>,
    turns: HashMap<String, Turn>,
    activity: HashMap<String, String>,
    queued: VecDeque<Command>,
    /// Streaming tails parked when the bounded event channel is full, keyed by
    /// runtime. The transport loop must not stall on UI consumption speed.
    /// Text tails carry their message id; reasoning tails carry the replace
    /// flag their flush must replay.
    pending_deltas: HashMap<String, PendingDelta>,
    pending_reasoning: HashMap<String, (bool, String)>,
    /// Sender for off-loop fetch results that need on-loop application;
    /// `run()` takes the receiving half before the select loop starts. Each
    /// result carries the connection epoch it was spawned on.
    work: tokio::sync::mpsc::UnboundedSender<StampedWork>,
    work_rx: Option<tokio::sync::mpsc::UnboundedReceiver<StampedWork>>,
    heartbeat: bool,
    ready: bool,
    opened_at: Option<Instant>,
    last_inbound: Instant,
    /// Whether the last disconnect was transient and worth retrying automatically.
    retryable: bool,
}

async fn run(
    config: Config,
    mut incoming: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: tokio::sync::mpsc::Sender<Event>,
) {
    let mut link = match Link::new(config, events.clone()) {
        Ok(link) => link,
        Err(message) => {
            let _ = events
                .send(Event::Connection(ConnectionState::Disconnected(message)))
                .await;
            return;
        }
    };
    if !open_link(&mut link, &mut incoming).await {
        link.close_socket().await;
        return;
    }
    // The link's off-loop fetch results re-enter the loop here; the channel
    // halves were created with the link, so this take is structural.
    let mut work_rx = link
        .work_rx
        .take()
        .expect("run owns the loop work channel once");
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    let mut attempts: u32 = 0;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let track = |link: &Link, attempts: &mut u32, retry_at: &mut Option<tokio::time::Instant>| {
        *retry_at = reconnect_deadline(
            link.write.is_some(),
            link.retryable,
            *retry_at,
            attempts,
            tokio::time::Instant::now(),
            jitter_subsec(),
        );
    };
    loop {
        let command = tokio::select! {
            biased;
            command = incoming.recv() => command,
            frame = next_frame(&mut link.read) => {
                link.on_socket(frame).await;
                track(&link, &mut attempts, &mut retry_at);
                continue;
            }
            _ = maintenance.tick() => {
                link.maintain().await;
                track(&link, &mut attempts, &mut retry_at);
                continue;
            }
            work = work_rx.recv() => {
                // A result whose epoch predates the current connection is
                // stale: its fetch ran under a socket that has since torn
                // down, so applying it would act on a dead runtime.
                if let Some(stamped) = work
                    && let Some(work) = link.live_work(stamped)
                {
                    match work {
                        LoopWork::Listed(listed) => {
                            link.apply_listed(*listed).await;
                        }
                        LoopWork::Uploaded {
                            runtime_id,
                            generation,
                            result,
                            prompt_text,
                        } => {
                            link.apply_uploaded(runtime_id, generation, result, prompt_text)
                                .await;
                        }
                        LoopWork::Mutated {
                            stored_id,
                            mutation,
                            result,
                        } => {
                            link.apply_mutated(stored_id, mutation, result).await;
                        }
                        LoopWork::Branch(outcome) => {
                            link.apply_branch(outcome.request, outcome.seeds).await;
                        }
                        LoopWork::Readonly(outcome) => {
                            link.apply_readonly(*outcome).await;
                        }
                        LoopWork::Resumed(page) => {
                            link.apply_resumed(*page).await;
                        }
                        LoopWork::Page(page) => {
                            link.apply_synced_page(*page).await;
                        }
                        LoopWork::Branched(page) => {
                            link.apply_branched(*page).await;
                        }
                        LoopWork::Archived {
                            stored_id,
                            retried,
                            result,
                        } => {
                            link.apply_archived(stored_id, retried, result).await;
                        }
                        LoopWork::Deferred {
                            stored_id,
                            title,
                            pinned,
                            result,
                        } => {
                            link.apply_deferred_patch(stored_id, title, pinned, result)
                                .await;
                        }
                    }
                }
                track(&link, &mut attempts, &mut retry_at);
                continue;
            }
            _ = retry_wait(retry_at) => {
                retry_at = None;
                if !open_link(&mut link, &mut incoming).await {
                    break;
                }
                track(&link, &mut attempts, &mut retry_at);
                continue;
            }
        };
        let Some(command) = command else { break };
        if matches!(command, Command::Shutdown) {
            link.close_socket().await;
            break;
        }
        if matches!(command, Command::Reconnect) {
            attempts = 0;
            retry_at = None;
            if !open_link(&mut link, &mut incoming).await {
                break;
            }
            track(&link, &mut attempts, &mut retry_at);
            continue;
        }
        link.handle(command).await;
        track(&link, &mut attempts, &mut retry_at);
    }
    link.close_socket().await;
}

/// Backoff between automatic reconnect attempts: 1s, 2s, 4s, 8s, 16s, then 30s
/// (the scheduled delay; equal jitter spreads each attempt across its second
/// half — see `jittered`).
fn reconnect_delay(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        3 => Duration::from_secs(4),
        4 => Duration::from_secs(8),
        5 => Duration::from_secs(16),
        _ => Duration::from_secs(30),
    }
}

/// Wall-clock sub-second nanoseconds for backoff jitter: cheap (vDSO), and
/// it only has to desynchronize clients — this is not a randomness source.
fn jitter_subsec() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.subsec_nanos())
}

/// Equal jitter: half the scheduled delay is fixed, half scales with
/// `subsec` (wall-clock nanoseconds, 0..1s). Clients dropped by the same
/// gateway event therefore retry over a spread — for the 30s cap, over a
/// 15s window — instead of all landing on the same instant.
fn jittered(delay: Duration, subsec: u32) -> Duration {
    let half = delay / 2;
    let offset = (half.as_nanos() as u64) * (subsec as u64) / 1_000_000_000;
    half + Duration::from_nanos(offset)
}

/// Reconnect bookkeeping. A live socket resets the backoff; a dropped,
/// still-retryable socket schedules the next attempt. Authentication
/// failures are never rescheduled.
fn reconnect_deadline(
    connected: bool,
    retryable: bool,
    pending: Option<tokio::time::Instant>,
    attempts: &mut u32,
    now: tokio::time::Instant,
    subsec: u32,
) -> Option<tokio::time::Instant> {
    if connected {
        *attempts = 0;
        return None;
    }
    if pending.is_some() || !retryable {
        return pending;
    }
    *attempts = attempts.saturating_add(1);
    Some(now + jittered(reconnect_delay(*attempts), subsec))
}

async fn retry_wait(retry_at: Option<tokio::time::Instant>) {
    match retry_at {
        Some(when) => tokio::time::sleep_until(when).await,
        None => std::future::pending().await,
    }
}

enum OpenStop {
    Shutdown,
    Ready,
    Failed,
    TimedOut,
    Restart,
}

/// How a command that arrives mid-connect affects the attempt.
enum MidConnect {
    /// Park the command; it drains once the gateway reports ready. Only the
    /// same allowlist the not-ready path enforces (`List`, `Create`,
    /// `Resume`, `SearchSessions`, `FetchStudio`) may wait: these replay
    /// against fresh state.
    Queue(Command),
    /// Fail the command immediately — it carries a runtime id or session
    /// action that belongs to the torn-down connection, so replaying it on
    /// the new connection would target dead state. The message matches the
    /// not-ready path exactly.
    Reject(Command),
    /// Abort the attempt and retry immediately.
    Restart,
    /// Stop connecting.
    Shutdown,
}

/// Results fetched off the transport loop that must be re-applied on it:
/// the session list (its listing caches live on the link) and attachment
/// uploads (the submit RPC waits for the uploaded paths).
enum LoopWork {
    Listed(Box<Result<Event, String>>),
    Mutated {
        stored_id: String,
        mutation: SessionMutation,
        result: Result<PatchOutcome, String>,
    },
    Uploaded {
        runtime_id: String,
        generation: u64,
        result: Result<VecDeque<String>, String>,
        prompt_text: String,
    },
    Branch(Box<BranchSeedsOutcome>),
    Readonly(Box<ReadonlyOutcome>),
    Resumed(Box<ResumedPage>),
    Page(Box<SyncedPage>),
    Branched(Box<BranchedPage>),
    Archived {
        stored_id: String,
        retried: bool,
        result: Result<PatchOutcome, String>,
    },
    Deferred {
        stored_id: String,
        title: Option<String>,
        pinned: Option<bool>,
        result: Result<PatchOutcome, String>,
    },
}

/// An off-loop work result stamped with the connection epoch it was spawned
/// on. A drop or reconnect bumps the link's epoch in `close_socket`, so
/// `run()` discards results whose epoch no longer matches: a fetch started
/// under one connection never applies state to the next (a stale resume
/// would otherwise adopt a turn for a runtime the new connection never
/// resumed).
struct StampedWork {
    epoch: u64,
    work: LoopWork,
}

/// The work channel together with the connection epoch it was spawned on.
/// Off-loop tasks deliver through the sink, so every result is stamped with
/// the live epoch; a task cannot forget the stamp, because the plain sender
/// type never reaches a task signature.
#[derive(Clone)]
struct WorkSink {
    sender: tokio::sync::mpsc::UnboundedSender<StampedWork>,
    epoch: u64,
}

impl WorkSink {
    fn deliver(&self, work: LoopWork) {
        let _ = self
            .sender
            .send(StampedWork { epoch: self.epoch, work });
    }
}

/// Zero-copy shape of the per-token delta frames. Streaming is the hot
/// path: a `message.delta` frame parses straight into borrowed fields —
/// no JSON DOM — and dispatches to the delta state machine directly.
/// Anything that does not match the shape (or any other frame) falls back
/// to the full `Value` parse in `on_text`, unchanged.
#[derive(serde::Deserialize)]
struct DeltaFrame<'a> {
    #[serde(borrow)]
    method: &'a str,
    params: DeltaParams<'a>,
}

#[derive(serde::Deserialize)]
struct DeltaParams<'a> {
    #[serde(borrow)]
    r#type: &'a str,
    #[serde(borrow)]
    session_id: &'a str,
    payload: DeltaPayload<'a>,
}

#[derive(serde::Deserialize)]
struct DeltaPayload<'a> {
    #[serde(borrow)]
    text: &'a str,
    #[serde(borrow, default)]
    turn_id: &'a str,
    #[serde(default)]
    segment: i64,
    #[serde(default)]
    offset: i64,
}

/// A branch request whose earlier pages are fetched off the transport loop.
struct BranchSeeds {
    stored_id: String,
    runtime_id: Option<String>,
    parent_title: String,
    visible_count: Option<usize>,
    custom_title: Option<String>,
    seed_fallback: Vec<SeedMessage>,
}

/// The fetched outcome for a [`BranchSeeds`] request.
struct BranchSeedsOutcome {
    request: BranchSeeds,
    seeds: Result<Vec<SeedMessage>, String>,
}

/// The saved transcript for a refused resume, with the session rows walked
/// during count discovery (the loop warms its listing caches from them).
struct ReadonlyHistory {
    before: usize,
    messages: Vec<ChatMessage>,
    warmed: Vec<SessionSummary>,
}

/// The fetched outcome for a refused-resume transcript load.
struct ReadonlyOutcome {
    stored_id: String,
    reason: String,
    result: Result<ReadonlyHistory, String>,
}

/// The saved page for a successful resume, fetched off the transport loop;
/// the continuation (Resumed event, live info, turn adoption, open-request
/// replay) applies on the loop with the original reply carried alongside.
struct ResumedPage {
    stored_id: String,
    runtime_id: Option<String>,
    before: usize,
    messages: Result<Vec<ChatMessage>, String>,
    result: Value,
}

/// Which user action a synced history page completes. Undo and compress
/// replies both reload the conversation's first page before their event.
enum PageAction {
    Undo {
        runtime_id: String,
        removed: usize,
        mode: RewindMode,
        prompt_text: String,
    },
    Compress {
        runtime_id: String,
        summary: CompressSummary,
    },
}

/// A synced history page fetched off the transport loop, plus the action it
/// completes when the rows return.
struct SyncedPage {
    stored_id: String,
    before: usize,
    messages: Result<Vec<ChatMessage>, String>,
    action: PageAction,
}

/// The branched conversation's first page, fetched off the transport loop;
/// on error or an empty page the fallback seed rides instead, and the
/// `Branched` event applies on the loop when the rows return.
struct BranchedPage {
    stored_id: String,
    runtime_id: String,
    parent_stored_id: String,
    parent_title: String,
    title: String,
    fallback_messages: Vec<ChatMessage>,
    model: Option<ModelIdentity>,
    before: usize,
    messages: Vec<ChatMessage>,
}

/// The borrowed fast path for streaming frames: `Some` when `text` is a
/// `message.delta` event frame, with every field borrowed straight out of
/// the input — no JSON DOM is built for the frame.
fn delta_frame_fields(
    text: &str,
) -> Option<(&str, &str, &str, i64, i64)> {
    let frame = serde_json::from_str::<DeltaFrame>(text).ok()?;
    if frame.method != "event" || frame.params.r#type != "message.delta" {
        return None;
    }
    Some((
        frame.params.session_id,
        frame.params.payload.text,
        frame.params.payload.turn_id,
        frame.params.payload.segment,
        frame.params.payload.offset,
    ))
}

fn mid_connect(command: Option<Command>) -> MidConnect {
    match command {
        None | Some(Command::Shutdown) => MidConnect::Shutdown,
        Some(Command::Reconnect) => MidConnect::Restart,
        Some(command) if is_queueable_while_connecting(&command) => MidConnect::Queue(command),
        Some(command) => MidConnect::Reject(command),
    }
}

async fn open_link(
    link: &mut Link,
    incoming: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> bool {
    let mut waiting = VecDeque::new();
    let mut rejected = VecDeque::new();
    loop {
        let stop = {
            let connection = link.connect();
            tokio::pin!(connection);
            let bound = tokio::time::sleep(CONNECT_TIMEOUT);
            tokio::pin!(bound);
            loop {
                tokio::select! {
                    biased;
                    command = incoming.recv() => match mid_connect(command) {
                        MidConnect::Shutdown => break OpenStop::Shutdown,
                        MidConnect::Restart => break OpenStop::Restart,
                        MidConnect::Queue(command) => waiting.push_back(command),
                        MidConnect::Reject(command) => rejected.push_back(command),
                    },
                    result = &mut connection => break match result {
                        Ok(()) => OpenStop::Ready,
                        Err(_) => OpenStop::Failed,
                    },
                    _ = &mut bound => break OpenStop::TimedOut,
                }
            }
        };
        // Leaving the block drops the in-flight attempt, cancelling its
        // HTTP requests and socket dial. The rejects can now reach the
        // link; they fail with the same message the not-ready path emits.
        while let Some(command) = rejected.pop_front() {
            link.reject_unready(command).await;
        }
        if matches!(stop, OpenStop::Shutdown) {
            link.queued.extend(waiting);
            return false;
        }
        if matches!(stop, OpenStop::TimedOut) {
            link.close_socket().await;
            link.emit(Event::Connection(ConnectionState::Disconnected(
                "The gateway connection timed out.".into(),
            )))
            .await;
        }
        if !matches!(stop, OpenStop::Restart) {
            link.queued.extend(waiting);
            return true;
        }
        // Restart: retry the sign-in immediately instead of waiting out the
        // connect bound against a stuck gateway.
    }
}

async fn next_frame(
    read: &mut Option<futures_util::stream::SplitStream<Socket>>,
) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
    match read.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

impl Link {
    fn new(config: Config, events: tokio::sync::mpsc::Sender<Event>) -> Result<Self, String> {
        let origin = config.url.clone();
        let http = http_client(&origin)?;
        let (work, work_rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            http,
            events,
            origin,
            username: config.username.clone(),
            password: config.password.clone(),
            secrets: vec![config.password, config.username],
            read: None,
            write: None,
            next_id: 1,
            next_turn_generation: 0,
            epoch: 0,
            message_counts: HashMap::new(),
            pinned_sessions: HashSet::new(),
            deferred_mutations: HashMap::new(),
            patching: HashSet::new(),
            jobs: HashMap::new(),
            runtime_of: HashMap::new(),
            refused: HashSet::new(),
            asks: HashMap::new(),
            turns: HashMap::new(),
            activity: HashMap::new(),
            queued: VecDeque::new(),
            heartbeat: false,
            ready: false,
            opened_at: None,
            last_inbound: Instant::now(),
            pending_deltas: HashMap::new(),
            pending_reasoning: HashMap::new(),
            work,
            work_rx: Some(work_rx),
            retryable: false,
        })
    }

    async fn emit(&self, event: Event) {
        let _ = self.events.send(event).await;
    }

    async fn fail(&self, operation: &str, session_id: Option<String>, message: impl Into<String>) {
        self.emit(Event::Error {
            operation: operation.into(),
            session_id,
            message: self.clean(message.into()),
        })
        .await;
    }

    fn clean(&self, text: String) -> String {
        sanitize(&text, &self.secrets)
    }

    /// Credential tokens (`password` and ephemeral `ws-ticket`) without `username`,
    /// so short usernames never corrupt skill category slugs or workspace labels.
    fn credential_secrets(&self) -> Vec<String> {
        let mut creds = Vec::with_capacity(2);
        if let Some(pw) = self.secrets.first() {
            creds.push(pw.clone());
        }
        for extra in self.secrets.iter().skip(2) {
            creds.push(extra.clone());
        }
        creds
    }

    async fn connect(&mut self) -> Result<(), String> {
        self.emit(Event::Connection(ConnectionState::Connecting))
            .await;
        self.ready = false;
        self.opened_at = None;
        // Transient by default; permanent refusals opt out below.
        self.retryable = true;
        self.close_socket().await;
        self.fail_jobs("The connection closed before the gateway answered.")
            .await;
        self.sign_in().await?;
        let ticket = self.request_ticket().await?;
        self.secrets.truncate(2);
        self.secrets.push(ticket.clone());
        let Some(url) = ws_dial_url(&self.origin, &ticket) else {
            self.retryable = false;
            self.emit(Event::Connection(ConnectionState::Disconnected(
                "Could not open the gateway socket.".into(),
            )))
            .await;
            return Err("scheme".into());
        };
        self.dial_socket(url).await
    }

    /// Password sign-in. Emits the connection state for every refusal; a
    /// rejected credential or login rate limit marks the failure permanent.
    async fn sign_in(&mut self) -> Result<(), String> {
        let login = self
            .http
            .post(endpoint(&self.origin, "/auth/password-login"))
            .json(&json!({
                "provider": "basic",
                "username": self.username,
                "password": self.password,
            }))
            .send()
            .await;
        let login = match login {
            Ok(response) => response,
            Err(_) => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "Could not reach the gateway.".into(),
                )))
                .await;
                return Err("Could not reach the gateway.".into());
            }
        };
        if login.status().is_success() {
            return Ok(());
        }
        let state = if login.status().as_u16() == 401 || login.status().as_u16() == 404 {
            self.retryable = false;
            ConnectionState::AuthenticationFailed(
                "The gateway rejected the username or password.".into(),
            )
        } else if login.status().as_u16() == 429 {
            self.retryable = false;
            ConnectionState::AuthenticationFailed(
                "Too many login attempts. Try again shortly.".into(),
            )
        } else {
            ConnectionState::Disconnected(format!(
                "Login failed (HTTP {}).",
                login.status().as_u16()
            ))
        };
        self.emit(Event::Connection(state)).await;
        Err("login failed".into())
    }

    /// Requests the fresh one-connection WebSocket ticket. A 401 here means the
    /// login session itself was rejected, which is permanent.
    async fn request_ticket(&mut self) -> Result<String, String> {
        let response = match self
            .http
            .post(endpoint(&self.origin, "/api/auth/ws-ticket"))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "Could not request a connection ticket.".into(),
                )))
                .await;
                return Err("ticket request failed".into());
            }
        };
        if response.status().as_u16() == 401 {
            self.retryable = false;
            self.emit(Event::Connection(ConnectionState::AuthenticationFailed(
                "The gateway did not accept the login session.".into(),
            )))
            .await;
            return Err("ticket unauthorized".into());
        }
        if !response.status().is_success() {
            self.emit(Event::Connection(ConnectionState::Disconnected(format!(
                "Could not request a connection ticket (HTTP {}).",
                response.status().as_u16()
            ))))
            .await;
            return Err("ticket failed".into());
        }
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(_) => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "The gateway ticket response was not usable.".into(),
                )))
                .await;
                return Err("ticket body".into());
            }
        };
        match body
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        {
            Some(ticket) => Ok(ticket),
            None => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "The gateway did not provide a connection ticket.".into(),
                )))
                .await;
                Err("missing ticket".into())
            }
        }
    }

    /// Dials the ticketed WebSocket and installs both halves as the live link.
    async fn dial_socket(&mut self, url: url::Url) -> Result<(), String> {
        let url_string: String = url.into();
        let connected =
            tokio::time::timeout(RPC_TIMEOUT, tokio_tungstenite::connect_async(&url_string)).await;
        drop(url_string);
        let (socket, _) = match connected {
            Ok(Ok(pair)) => pair,
            _ => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "Could not open the gateway socket.".into(),
                )))
                .await;
                return Err("socket".into());
            }
        };
        let (write, read) = socket.split();
        self.write = Some(write);
        self.read = Some(read);
        self.last_inbound = Instant::now();
        self.heartbeat = false;
        self.ready = false;
        self.retryable = true;
        self.opened_at = Some(Instant::now());
        Ok(())
    }

    async fn finish_ready(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        self.opened_at = None;
        if self
            .send_rpc(
                "client.capabilities",
                json!({"server_requests": true}),
                JobKind::Capabilities,
                RPC_TIMEOUT,
            )
            .await
            .is_err()
        {
            return;
        }
        self.emit(Event::Connection(ConnectionState::Connected))
            .await;
        self.fetch_models().await;
        // Drained commands run before the studio catalogs: after a reconnect the
        // user's queued actions must not wait on two HTTP round-trips.
        while self.ready && self.write.is_some() {
            let Some(command) = self.queued.pop_front() else {
                break;
            };
            self.handle(command).await;
            if !self.ready || self.write.is_none() {
                break;
            }
        }
        self.fetch_studio_catalogs();
    }

    async fn handle(&mut self, command: Command) {
        if (!self.ready || self.write.is_none())
            && !matches!(command, Command::Reconnect | Command::Shutdown)
        {
            self.handle_unready_command(command).await;
            return;
        }
        match command {
            Command::Reconnect | Command::Shutdown => {}
            Command::List { .. }
            | Command::History { .. }
            | Command::SearchSessions { .. }
            | Command::MutateSession { .. }
            | Command::FetchStudio => self.http_command(command).await,
            Command::Create { title, cwd } => self.begin_create(title, cwd).await,
            Command::Resume { stored_id } => self.begin_resume(stored_id).await,
            Command::Submit {
                runtime_id,
                text,
                attachments,
            } => {
                self.submit_unless_stopping(runtime_id, text, attachments)
                    .await;
            }
            Command::Steer { runtime_id, text } => self.begin_steer(runtime_id, text).await,
            Command::Interrupt { runtime_id } => self.begin_interrupt(runtime_id).await,
            Command::Respond {
                runtime_id,
                request_id,
                answer,
            } => self.respond(runtime_id, request_id, answer).await,
            Command::Delete { stored_id } => self.begin_delete(stored_id).await,
            Command::Branch { .. } | Command::Undo { .. } | Command::Compress { .. } => {
                self.timeline_command(command).await;
            }
            Command::FetchModels | Command::SetModel { .. } | Command::AskSide { .. } => {
                self.model_command(command).await;
            }
        }
    }

    async fn submit_unless_stopping(
        &mut self,
        runtime_id: String,
        text: String,
        attachments: Vec<StagedImage>,
    ) {
        if !self
            .turns
            .get(&runtime_id)
            .is_some_and(|turn| turn.phase == TurnPhase::Stopping)
        {
            self.begin_submit(runtime_id, text, attachments).await;
        }
    }

    async fn http_command(&mut self, command: Command) {
        match command {
            Command::List { offset } => self.list(offset),
            Command::History { stored_id, before } => self.history(stored_id, before),
            Command::SearchSessions { query, request_seq } => {
                self.search_sessions(query, request_seq).await;
            }
            Command::MutateSession {
                stored_id,
                mutation,
            } => self.mutate_session(stored_id, mutation).await,
            Command::FetchStudio => self.fetch_studio_catalogs(),
            _ => {}
        }
    }

    async fn handle_unready_command(&mut self, command: Command) {
        if self.write.is_some() || is_queueable_while_connecting(&command) {
            self.queued.push_back(command);
            return;
        }
        self.reject_unready(command).await;
    }

    /// Emits the immediate failure for a command that cannot wait for a
    /// connection. The not-ready and mid-connect paths share it, so a
    /// Submit or Steer issued while disconnected gets the same answer in
    /// both states instead of replaying against a dead runtime.
    async fn reject_unready(&mut self, command: Command) {
        if let Command::AskSide {
            runtime_id,
            client_id,
            ..
        } = command
        {
            self.emit(Event::SideFailed {
                runtime_id,
                client_id,
                message: "Not connected. Reconnect to try again.".into(),
            })
            .await;
            return;
        }
        if let Some((op, stored_id)) = unready_session_failure(&command) {
            self.fail(
                op,
                Some(stored_id),
                "Not connected. Reconnect to try again.",
            )
            .await;
            return;
        }
        self.fail("connect", None, "Not connected. Reconnect to try again.")
            .await;
    }

    async fn begin_create(&mut self, title: String, cwd: Option<String>) {
        let params = create_rpc_params(&title, cwd.as_deref());
        if let Err(fault) = self
            .send_rpc(
                "session.create",
                params,
                JobKind::Create { title },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail("create", None, send_fault_message("create", fault))
                .await;
        }
    }

    async fn begin_resume(&mut self, stored_id: String) {
        if let Err(fault) = self
            .send_rpc(
                "session.resume",
                json!({
                    "session_id": stored_id,
                    "inline_images": false,
                    "omit_messages": true
                }),
                JobKind::Resume {
                    stored_id: stored_id.clone(),
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "resume",
                Some(stored_id),
                send_fault_message("resume", fault),
            )
            .await;
        }
    }

    async fn begin_steer(&mut self, runtime_id: String, text: String) {
        match steer_route(self.turns.get(&runtime_id)) {
            SteerRoute::Drop => {}
            SteerRoute::Redirect => {
                if let Err(fault) = self
                    .send_rpc(
                        "session.redirect",
                        json!({"session_id": runtime_id, "text": text}),
                        JobKind::Steer {
                            runtime_id: runtime_id.clone(),
                            text,
                        },
                        RPC_TIMEOUT,
                    )
                    .await
                {
                    // A failed steer must not end the live turn: the
                    // "steer" operation is deliberately not "submit".
                    self.fail(
                        "steer",
                        Some(runtime_id),
                        send_fault_message("steer", fault),
                    )
                    .await;
                }
            }
            // Idle or already-settled turn: this is a normal next prompt.
            SteerRoute::Submit => self.begin_submit(runtime_id, text, Vec::new()).await,
        }
    }

    async fn begin_interrupt(&mut self, runtime_id: String) {
        let generation = self
            .turns
            .get(&runtime_id)
            .map_or(0, |turn| turn.generation);
        if let Err(fault) = self
            .send_rpc(
                "session.interrupt",
                json!({"session_id": runtime_id}),
                JobKind::Interrupt {
                    runtime_id: runtime_id.clone(),
                    generation,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "interrupt",
                Some(runtime_id),
                send_fault_message("interrupt", fault),
            )
            .await;
        }
    }

    async fn begin_submit(
        &mut self,
        runtime_id: String,
        text: String,
        attachments: Vec<StagedImage>,
    ) {
        self.next_turn_generation += 1;
        let generation = self.next_turn_generation;
        self.turns.insert(
            runtime_id.clone(),
            Turn::submitting(format!("turn-{runtime_id}-{generation}").into(), generation),
        );
        let mut wire_text = studio::effective_wire_prompt(&text, !attachments.is_empty());
        for file in attachments.iter().filter(|file| !file.is_image()) {
            wire_text.push_str(&format!(
                "\n\n--- Attached file: {} ---\n{}\n--- End attached file ---",
                file.filename,
                String::from_utf8_lossy(&file.bytes)
            ));
        }
        if attachments.is_empty() {
            self.send_submit_rpc(runtime_id, generation, wire_text)
                .await;
            return;
        }
        // Image uploads run off the transport loop and concurrently: a slow
        // upload must not stall frames, and N images no longer cost N
        // serial round trips. The result re-enters via the work channel,
        // where the generation guard discards stale uploads.
        let images: Vec<StagedImage> = attachments
            .into_iter()
            .filter(|attachment| attachment.is_image())
            .collect();
        if images.is_empty() {
            self.advance_attach_or_submit(runtime_id, generation, VecDeque::new(), wire_text)
                .await;
            return;
        }
        tokio::spawn(upload_images_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            runtime_id,
            generation,
            images,
            wire_text,
        ));
    }

    /// Applies a finished upload set on the loop: the submit RPC proceeds
    /// with the uploaded paths, or the turn fails exactly as the inline
    /// path did.
    async fn apply_uploaded(
        &mut self,
        runtime_id: String,
        generation: u64,
        result: Result<VecDeque<String>, String>,
        prompt_text: String,
    ) {
        match result {
            Ok(paths) => {
                self.advance_attach_or_submit(runtime_id, generation, paths, prompt_text)
                    .await;
            }
            Err(err) => {
                if let Some(turn) = self.turns.get_mut(&runtime_id) {
                    let _ = turn.fail_submit(generation);
                }
                self.fail(
                    "submit",
                    Some(runtime_id),
                    format!("{err} The prompt was not sent."),
                )
                .await;
            }
        }
    }

    async fn advance_attach_or_submit(
        &mut self,
        runtime_id: String,
        generation: u64,
        mut remaining_paths: VecDeque<String>,
        prompt_text: String,
    ) {
        let still_submitting = self.turns.get(&runtime_id).is_some_and(|turn| {
            turn.generation == generation
                && turn.phase == TurnPhase::Submitting
                && !turn.submit_settled
        });
        if !still_submitting {
            return;
        }
        let Some(next_path) = remaining_paths.pop_front() else {
            self.send_submit_rpc(runtime_id, generation, prompt_text)
                .await;
            return;
        };
        if let Err(fault) = self
            .send_rpc(
                "image.attach",
                json!({"session_id": runtime_id, "path": next_path}),
                JobKind::AttachImage {
                    runtime_id: runtime_id.clone(),
                    generation,
                    remaining_paths,
                    prompt_text,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.abort_local_submit(&runtime_id, generation, fault)
                .await;
        }
    }

    async fn send_submit_rpc(&mut self, runtime_id: String, generation: u64, text: String) {
        if let Err(fault) = self
            .send_rpc(
                "prompt.submit",
                json!({"session_id": runtime_id, "text": text}),
                JobKind::Submit {
                    runtime_id: runtime_id.clone(),
                    generation,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.abort_local_submit(&runtime_id, generation, fault)
                .await;
        }
    }

    async fn abort_local_submit(&mut self, runtime_id: &str, generation: u64, fault: SendFault) {
        if let Some(turn) = self.turns.get_mut(runtime_id) {
            let _ = turn.fail_submit(generation);
        }
        self.fail(
            "submit",
            Some(runtime_id.to_string()),
            send_fault_message("submit", fault),
        )
        .await;
    }

    async fn model_command(&mut self, command: Command) {
        match command {
            Command::FetchModels => self.fetch_models().await,
            Command::SetModel {
                runtime_id,
                prime,
                target,
                confirm_expensive,
            } => {
                if let Some(prime) = prime {
                    self.begin_prime(runtime_id, prime, target, confirm_expensive)
                        .await;
                } else {
                    self.begin_switch(runtime_id, target, confirm_expensive)
                        .await;
                }
            }
            Command::AskSide {
                runtime_id,
                client_id,
                question,
            } => {
                self.begin_side_question(runtime_id, client_id, question)
                    .await;
            }
            _ => {}
        }
    }

    async fn begin_side_question(&mut self, runtime_id: String, client_id: u64, question: String) {
        if let Err(fault) = self
            .send_rpc(
                "prompt.btw",
                json!({"session_id": runtime_id, "text": question}),
                JobKind::SideQuestion {
                    runtime_id: runtime_id.clone(),
                    client_id,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.emit(Event::SideFailed {
                runtime_id,
                client_id,
                message: send_fault_message("btw", fault),
            })
            .await;
        }
    }

    async fn timeline_command(&mut self, command: Command) {
        match command {
            Command::Branch {
                stored_id,
                runtime_id,
                parent_title,
                before_loaded,
                visible_count,
                custom_title,
                seed_fallback,
            } => {
                self.begin_branch(
                    BranchSeeds {
                        stored_id,
                        runtime_id,
                        parent_title,
                        visible_count,
                        custom_title,
                        seed_fallback,
                    },
                    before_loaded,
                )
                .await;
            }
            Command::Undo {
                stored_id,
                runtime_id,
                mode,
                prompt_text,
            } => {
                self.begin_undo(stored_id, runtime_id, mode, prompt_text)
                    .await;
            }
            Command::Compress {
                stored_id,
                runtime_id,
            } => {
                self.begin_compress(stored_id, runtime_id).await;
            }
            _ => {}
        }
    }

    async fn begin_compress(&mut self, stored_id: String, runtime_id: String) {
        if let Err(fault) = self
            .send_rpc(
                "session.compress",
                json!({"session_id": runtime_id}),
                JobKind::Compress {
                    stored_id: stored_id.clone(),
                    runtime_id,
                },
                COMPRESS_TIMEOUT,
            )
            .await
        {
            self.fail(
                "compress",
                Some(stored_id),
                send_fault_message("compress", fault),
            )
            .await;
        }
    }

    /// Catalog fetches run off the transport loop: two HTTP round-trips with
    /// 30s timeouts must not stall frame processing and probe answering right
    /// after `gateway.ready`. The task emits the same events on the bounded
    /// channel; a reconnect just spawns a fresh fetch that supersedes it.
    fn fetch_studio_catalogs(&self) {
        tokio::spawn(fetch_studio_catalogs_task(
            self.http.clone(),
            self.origin.clone(),
            self.credential_secrets(),
            self.secrets.clone(),
            self.events.clone(),
        ));
    }

    async fn begin_branch(&mut self, request: BranchSeeds, before_loaded: usize) {
        if before_loaded > 0
            && (request.visible_count.is_some() || request.runtime_id.is_none())
        {
            // Earlier pages are fetched off the transport loop — a slow or
            // stuck history endpoint must not stall frames — and the branch
            // RPC applies when the seeds arrive.
            tokio::spawn(branch_seeds_task(
                self.http.clone(),
                self.origin.clone(),
                before_loaded,
                self.work_sink(),
                request,
            ));
            return;
        }
        self.apply_branch(request, Ok(Vec::new())).await;
    }

    /// Sends the branch RPC with fetched earlier pages (empty when none were
    /// needed) — the same shape, seeds, and failure messages the inline
    /// path produced.
    async fn apply_branch(
        &mut self,
        request: BranchSeeds,
        seeds: Result<Vec<SeedMessage>, String>,
    ) {
        let (total_visible, full_seed) = match seeds {
            Ok(mut earlier) => {
                let prefix_len = earlier.len();
                earlier.extend(request.seed_fallback);
                (request.visible_count.map(|v| prefix_len + v), earlier)
            }
            Err(err) => {
                self.fail("branch", Some(request.stored_id.clone()), err)
                    .await;
                return;
            }
        };
        let fallback_title =
            default_branch_title(&request.parent_title, request.custom_title.as_deref());
        if let Some(rt) = request.runtime_id {
            let params = branch_rpc_params(&rt, request.custom_title.as_deref(), total_visible);
            if let Err(fault) = self
                .send_rpc(
                    "session.branch",
                    params,
                    JobKind::Branch {
                        parent_stored_id: request.stored_id.clone(),
                        parent_title: request.parent_title,
                        fallback_title,
                        fallback_seed: full_seed,
                    },
                    RPC_TIMEOUT,
                )
                .await
            {
                self.fail(
                    "branch",
                    Some(request.stored_id),
                    send_fault_message("branch", fault),
                )
                .await;
            }
        } else if total_visible.is_none() && request.custom_title.is_none() {
            self.begin_stored_branch(
                request.stored_id,
                request.parent_title,
                fallback_title,
                full_seed,
            )
            .await;
        } else {
            self.begin_seeded_branch(
                request.stored_id,
                request.parent_title,
                fallback_title,
                full_seed,
            )
            .await;
        }
    }

    async fn begin_stored_branch(
        &mut self,
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_seed: Vec<SeedMessage>,
    ) {
        if let Err(fault) = self
            .send_rpc(
                "session.branch_stored",
                json!({"parent_session_id": parent_stored_id}),
                JobKind::BranchStored {
                    parent_stored_id: parent_stored_id.clone(),
                    parent_title,
                    fallback_title,
                    fallback_seed,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "branch",
                Some(parent_stored_id),
                send_fault_message("branch", fault),
            )
            .await;
        }
    }

    async fn begin_seeded_branch(
        &mut self,
        parent_stored_id: String,
        parent_title: String,
        title: String,
        seed: Vec<SeedMessage>,
    ) {
        if seed.is_empty() {
            self.fail(
                "branch",
                Some(parent_stored_id),
                "Nothing to branch — send a message first.",
            )
            .await;
            return;
        }
        let params = seeded_branch_params(&title, &parent_stored_id, &seed);
        let seed_count = seed.len();
        if let Err(fault) = self
            .send_rpc(
                "session.create",
                params,
                JobKind::BranchSeeded {
                    parent_stored_id: parent_stored_id.clone(),
                    parent_title,
                    title,
                    seed_count,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "branch",
                Some(parent_stored_id),
                send_fault_message("branch", fault),
            )
            .await;
        }
    }

    async fn begin_undo(
        &mut self,
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
    ) {
        self.send_undo(stored_id, runtime_id, mode, prompt_text, 0)
            .await;
    }

    async fn send_undo(
        &mut self,
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
        attempts: u8,
    ) {
        if let Err(fault) = self
            .send_rpc(
                "session.undo",
                undo_rpc_params(&runtime_id, mode),
                JobKind::Undo {
                    stored_id: stored_id.clone(),
                    runtime_id,
                    mode,
                    prompt_text,
                    attempts,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail("undo", Some(stored_id), send_fault_message("undo", fault))
                .await;
        }
    }

    async fn fetch_models(&mut self) {
        if let Err(fault) = self
            .send_rpc(
                "model.options",
                json!({
                    "explicit_only": true,
                    "include_unconfigured": false,
                    "refresh": false
                }),
                JobKind::Catalog,
                RPC_TIMEOUT,
            )
            .await
        {
            let _ = fault;
            self.emit(Event::Catalog {
                catalog: Catalog {
                    error: Some("The model catalog could not be requested.".into()),
                    ..Default::default()
                },
            })
            .await;
        }
    }

    /// The gateway skips waiting for an in-progress agent build when the switch names
    /// an explicit provider, so a lazy chat needs a provider-less call first: that one
    /// starts and waits for the build the real switch then applies to.
    async fn begin_prime(
        &mut self,
        runtime_id: String,
        prime: ModelIdentity,
        target: ModelIdentity,
        confirm_expensive: bool,
    ) {
        if let Err(fault) = self
            .send_rpc(
                "config.set",
                json!({
                    "session_id": runtime_id,
                    "key": "model",
                    "value": prime.prime_value(),
                    "confirm_expensive_model": false
                }),
                JobKind::Prime {
                    runtime_id: runtime_id.clone(),
                    target,
                    confirm_expensive,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            let _ = fault;
            self.emit(Event::ModelAmbiguous { runtime_id }).await;
        }
    }

    async fn begin_switch(
        &mut self,
        runtime_id: String,
        target: ModelIdentity,
        confirm_expensive: bool,
    ) {
        if let Err(fault) = self
            .send_rpc(
                "config.set",
                json!({
                    "session_id": runtime_id,
                    "key": "model",
                    "value": target.switch_value(),
                    "confirm_expensive_model": confirm_expensive
                }),
                JobKind::Switch {
                    runtime_id: runtime_id.clone(),
                    target,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            let _ = fault;
            self.emit(Event::ModelAmbiguous { runtime_id }).await;
        }
    }

    /// One switch reply. Scope is mandatory: an unscoped reply may have written the
    /// profile default, which this client never accepts as a per-chat change.
    async fn model_reply(&mut self, runtime_id: String, target: ModelIdentity, result: &Value) {
        match crate::model::interpret_switch(&target, result) {
            crate::model::SwitchReply::Rejected(message) => {
                self.emit(Event::ModelRejected {
                    runtime_id,
                    message,
                })
                .await;
            }
            crate::model::SwitchReply::NeedsConfirmation(message) => {
                self.emit(Event::ModelConfirmation {
                    runtime_id,
                    target,
                    message: self.clean(message),
                })
                .await;
            }
            crate::model::SwitchReply::Accepted { resolved, deferred } => {
                self.emit(Event::ModelAck {
                    runtime_id,
                    resolved,
                    deferred,
                })
                .await;
            }
        }
    }

    /// The session list fetch runs off the transport loop — a slow or stuck
    /// gateway must not stall frames here either (it is the first command of
    /// the post-reconnect refresh). The fetched rows re-enter the loop via
    /// the work channel, where the listing caches and cleaning live.
    fn list(&self, offset: usize) {
        let work = self.work_sink();
        let http = self.http.clone();
        let origin = self.origin.clone();
        tokio::spawn(async move {
            work.deliver(LoopWork::Listed(Box::new(
                fetch_sessions(&http, &origin, offset).await,
            )));
        });
    }

    /// Applies a fetched session list on the loop: title/preview cleaning
    /// plus the listing caches, then the same event the inline path emitted.
    async fn apply_listed(&mut self, listed: Result<Event, String>) {
        match listed {
            Ok(mut event) => {
                if let Event::Sessions { sessions, .. } = &mut event {
                    self.note_and_clean_listed(sessions);
                }
                self.emit(event).await;
            }
            Err(message) => self.fail("list", None, message).await,
        }
    }

    fn note_and_clean_listed(&mut self, sessions: &mut [SessionSummary]) {
        for session in sessions {
            session.title = self.clean(std::mem::take(&mut session.title));
            session.preview = self.clean(std::mem::take(&mut session.preview));
            self.note_listed(session);
        }
    }

    /// Search hits ride the same off-loop task shape as the catalog and
    /// history reads; the pinned set is cloned so marking stays identical.
    async fn search_sessions(&self, query: String, request_seq: u64) {
        if query.trim().is_empty() {
            self.emit(Event::SearchResults {
                query,
                request_seq,
                hits: Vec::new(),
            })
            .await;
            return;
        }
        tokio::spawn(fetch_search_task(
            self.http.clone(),
            self.origin.clone(),
            self.secrets.clone(),
            self.pinned_sessions.clone(),
            self.events.clone(),
            query,
            request_seq,
        ));
    }

    async fn mutate_session(&mut self, stored_id: String, mutation: SessionMutation) {
        if matches!(mutation, SessionMutation::Archive) {
            self.begin_archive(stored_id).await;
            return;
        }
        if self.patching.contains(&stored_id) {
            // A patch for this conversation is already in flight — a user
            // patch or a deferred flush. A second concurrent patch would
            // race the fenced revision and surface HTTP 409 to the user, so
            // the new mutation rides the deferral and flushes when the
            // in-flight patch resolves.
            self.absorb_into_deferral(stored_id, mutation).await;
            return;
        }
        // The patch — including its one conflict retry — runs off the
        // transport loop; the outcome applies where curation state lives.
        let body = recall::mutation_patch_body(&mutation);
        self.patching.insert(stored_id.clone());
        tokio::spawn(patch_session_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            stored_id,
            mutation,
            body,
        ));
    }

    /// Applies a finished session patch on the loop: the same outcome
    /// mapping the inline path used, including the unpersisted-live guard
    /// that only the loop can evaluate.
    async fn apply_mutated(
        &mut self,
        stored_id: String,
        mutation: SessionMutation,
        result: Result<PatchOutcome, String>,
    ) {
        self.patching.remove(&stored_id);
        let chained = stored_id.clone();
        match result {
            Ok(PatchOutcome::Applied) => {
                self.finish_applied_mutation(stored_id, mutation).await;
                // A mutation absorbed while this patch was in flight rides
                // the deferral; with the patch applied, flush it now.
                // Failure arms do not chain: the next real trigger (a
                // completed message) retries the absorbed changes.
                self.chain_deferred_flush(&chained);
            }
            Ok(PatchOutcome::NotFound) if self.is_unpersisted_live(&stored_id) => {
                self.apply_unpersisted_mutation(stored_id, mutation).await;
            }
            Ok(PatchOutcome::NotFound) => {
                self.fail("curation", Some(stored_id), "Session not found.")
                    .await;
            }
            Ok(PatchOutcome::Conflict) => {
                self.fail(
                    "curation",
                    Some(stored_id),
                    "Could not update active conversation (HTTP 409).",
                )
                .await;
            }
            Err(message) => self.fail("curation", Some(stored_id), message).await,
        }
    }

    /// Records a mutation into the deferral map and shows it as applied:
    /// the pin cache updates, the entry waits for a flush trigger, and the
    /// UI sees success immediately. Shared by the unpersisted path and the
    /// in-flight-patch collision path.
    async fn absorb_into_deferral(&mut self, stored_id: String, mutation: SessionMutation) {
        if let SessionMutation::SetPinned(pinned) = mutation {
            self.set_pinned_cache(&stored_id, pinned);
        }
        self.record_deferred_mutation(&stored_id, &mutation);
        self.emit(Event::SessionMutated {
            stored_id,
            mutation,
        })
        .await;
    }

    async fn begin_archive(&mut self, stored_id: String) {
        if self.is_unpersisted_live(&stored_id) {
            self.apply_unpersisted_mutation(stored_id, SessionMutation::Archive)
                .await;
            return;
        }
        if let Some(runtime_id) = self.runtime_of.remove(&stored_id)
            && self
                .close_runtime_for_archive(&stored_id, &runtime_id, false)
                .await
        {
            return;
        }
        self.request_archive_patch(stored_id, false).await;
    }

    async fn close_runtime_for_archive(
        &mut self,
        stored_id: &str,
        runtime_id: &str,
        retried: bool,
    ) -> bool {
        let _ = self
            .send_rpc(
                "session.interrupt",
                json!({"session_id": runtime_id}),
                JobKind::Ping,
                RPC_TIMEOUT,
            )
            .await;
        self.send_rpc(
            "session.close",
            json!({"session_id": runtime_id}),
            JobKind::ArchiveClose {
                stored_id: stored_id.to_string(),
                retried,
            },
            RPC_TIMEOUT,
        )
        .await
        .is_ok()
    }

    async fn request_archive_patch(&mut self, stored_id: String, retried: bool) {
        let body = recall::mutation_patch_body(&SessionMutation::Archive);
        // The archive patch runs off the transport loop; the outcome maps
        // on the loop, where archive-completion state lives.
        tokio::spawn(archive_patch_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            stored_id,
            retried,
            body,
        ));
    }

    /// Maps a finished archive patch on the loop — the same outcomes the
    /// inline path produced, including the zero-message NotFound guard.
    async fn apply_archived(
        &mut self,
        stored_id: String,
        retried: bool,
        result: Result<PatchOutcome, String>,
    ) {
        match result {
            Ok(PatchOutcome::Applied) => self.finish_archive(stored_id).await,
            Ok(PatchOutcome::NotFound)
                if self.message_counts.get(&stored_id).copied() == Some(0) =>
            {
                self.finish_archive(stored_id).await;
            }
            Ok(PatchOutcome::NotFound) => {
                self.fail("curation", Some(stored_id), "Session not found.")
                    .await;
            }
            Ok(PatchOutcome::Conflict) => {
                self.retry_archive_after_conflict(stored_id, retried).await;
            }
            Err(message) => self.fail("curation", Some(stored_id), message).await,
        }
    }

    async fn finish_archive(&mut self, stored_id: String) {
        self.runtime_of.remove(&stored_id);
        self.pinned_sessions.remove(&stored_id);
        self.deferred_mutations.remove(&stored_id);
        self.emit(Event::SessionMutated {
            stored_id,
            mutation: SessionMutation::Archive,
        })
        .await;
    }

    async fn retry_archive_after_conflict(&mut self, stored_id: String, retried: bool) {
        if !retried
            && !self.refused.contains(&stored_id)
            && self
                .send_rpc(
                    "session.resume",
                    json!({
                        "session_id": stored_id,
                        "inline_images": false,
                        "omit_messages": true
                    }),
                    JobKind::ArchiveResume {
                        stored_id: stored_id.clone(),
                    },
                    RPC_TIMEOUT,
                )
                .await
                .is_ok()
        {
            return;
        }
        self.fail(
            "curation",
            Some(stored_id),
            "Could not archive active conversation (HTTP 409).",
        )
        .await;
    }

    fn set_pinned_cache(&mut self, stored_id: &str, pinned: bool) {
        if pinned {
            self.pinned_sessions.insert(stored_id.to_string());
        } else {
            self.pinned_sessions.remove(stored_id);
        }
    }

    async fn finish_applied_mutation(&mut self, stored_id: String, mutation: SessionMutation) {
        if let SessionMutation::SetPinned(pinned) = mutation {
            self.set_pinned_cache(&stored_id, pinned);
        }
        self.clear_deferred_field(&stored_id, &mutation);
        self.emit(Event::SessionMutated {
            stored_id,
            mutation,
        })
        .await;
    }

    fn is_unpersisted_live(&self, stored_id: &str) -> bool {
        self.runtime_of.contains_key(stored_id)
            && self.message_counts.get(stored_id).copied().unwrap_or(0) == 0
    }

    async fn apply_unpersisted_mutation(&mut self, stored_id: String, mutation: SessionMutation) {
        if matches!(mutation, SessionMutation::Archive) {
            self.pinned_sessions.remove(&stored_id);
            self.deferred_mutations.remove(&stored_id);
            self.patching.remove(&stored_id);
            self.close_live_runtime_quietly(&stored_id).await;
            self.emit(Event::SessionMutated {
                stored_id,
                mutation,
            })
            .await;
        } else {
            self.absorb_into_deferral(stored_id, mutation).await;
        }
    }

    fn record_deferred_mutation(&mut self, stored_id: &str, mutation: &SessionMutation) {
        let entry = self
            .deferred_mutations
            .entry(stored_id.to_string())
            .or_insert((None, None));
        match mutation {
            SessionMutation::Rename(title) => entry.0 = Some(title.trim().to_string()),
            SessionMutation::SetPinned(pinned) => entry.1 = Some(*pinned),
            SessionMutation::Archive => {}
        }
    }

    fn clear_deferred_field(&mut self, stored_id: &str, mutation: &SessionMutation) {
        let Some(entry) = self.deferred_mutations.get_mut(stored_id) else {
            return;
        };
        match mutation {
            SessionMutation::Rename(_) => entry.0 = None,
            SessionMutation::SetPinned(_) => entry.1 = None,
            SessionMutation::Archive => {}
        }
        if entry.0.is_none() && entry.1.is_none() {
            self.deferred_mutations.remove(stored_id);
        }
    }

    async fn close_live_runtime_quietly(&mut self, stored_id: &str) {
        if let Some(runtime_id) = self.runtime_of.remove(stored_id) {
            let _ = self
                .send_rpc(
                    "session.close",
                    json!({"session_id": runtime_id}),
                    JobKind::Ping,
                    RPC_TIMEOUT,
                )
                .await;
        }
    }

    fn flush_deferred_for_runtime(&mut self, runtime_id: &str) {
        let Some(stored_id) = self.stored_id_for_runtime(runtime_id) else {
            return;
        };
        self.flush_deferred(&stored_id);
    }

    /// Sends one conversation's deferred title/pin to the gateway, now that
    /// its first message has completed. Skips while any patch for the same
    /// conversation is in flight (the deferral retries on the next trigger)
    /// and marks its own patch in flight, so a concurrent user mutation
    /// rides the deferral instead of racing the fenced revision.
    fn flush_deferred(&mut self, stored_id: &str) {
        let count = self.message_counts.entry(stored_id.to_string()).or_insert(0);
        *count = (*count).max(2);
        if self.patching.contains(stored_id) {
            return;
        }
        let Some((title, pinned)) = self.deferred_mutations.remove(stored_id) else {
            return;
        };
        let Some(body) = deferred_patch_body(title.clone(), pinned) else {
            return;
        };
        self.patching.insert(stored_id.to_string());
        tokio::spawn(deferred_patch_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            stored_id.to_string(),
            title,
            pinned,
            body,
        ));
    }

    /// Flushes a deferred mutation that arrived while a patch for the same
    /// conversation was in flight: that patch window has closed, so the
    /// absorbed changes get their turn now.
    fn chain_deferred_flush(&mut self, stored_id: &str) {
        // The unpersisted case is excluded: a 404 there means the gateway
        // has no row yet, so an immediate re-patch would fail again — the
        // deferral waits for the real trigger (a completed message).
        if !self.is_unpersisted_live(stored_id)
            && self.runtime_of.contains_key(stored_id)
            && self.deferred_mutations.contains_key(stored_id)
        {
            self.flush_deferred(stored_id);
        }
    }

    /// Applies a finished deferred patch on the loop. Success is silent (the
    /// UI already showed the mutation when it was deferred); a refused save
    /// re-defers the title/pin — the user's intent survives and the next
    /// flush trigger retries — and surfaces, so a rename or pin can no
    /// longer silently revert on the next list refresh. A transport failure
    /// re-defers without an event: it is transient by nature and event spam
    /// per message would be noise.
    async fn apply_deferred_patch(
        &mut self,
        stored_id: String,
        title: Option<String>,
        pinned: Option<bool>,
        result: Result<PatchOutcome, String>,
    ) {
        self.patching.remove(&stored_id);
        match result {
            Ok(PatchOutcome::Applied) => {
                // A mutation absorbed while this flush was in flight rides
                // the deferral; with the save applied, flush it now.
                // Failure arms do not chain: a permanent refusal would loop,
                // and the next real trigger (a completed message) retries.
                self.chain_deferred_flush(&stored_id);
            }
            Ok(PatchOutcome::NotFound) => {
                self.reinsert_deferred(&stored_id, title, pinned);
                self.fail(
                    "curation",
                    Some(stored_id.clone()),
                    "Could not save conversation changes (HTTP 404).",
                )
                .await;
            }
            Ok(PatchOutcome::Conflict) => {
                self.reinsert_deferred(&stored_id, title, pinned);
                self.fail(
                    "curation",
                    Some(stored_id.clone()),
                    "Could not save conversation changes (HTTP 409).",
                )
                .await;
            }
            Err(_) => {
                self.reinsert_deferred(&stored_id, title, pinned);
            }
        }
    }

    fn reinsert_deferred(&mut self, stored_id: &str, title: Option<String>, pinned: Option<bool>) {
        // Merge, not overwrite: anything already in the map was absorbed
        // while this patch was in flight, so it is the user's LATER intent
        // for that field; only fill fields nothing newer has claimed.
        let entry = self
            .deferred_mutations
            .entry(stored_id.to_string())
            .or_insert((None, None));
        if entry.0.is_none() {
            entry.0 = title;
        }
        if entry.1.is_none() {
            entry.1 = pinned;
        }
    }

    fn stored_id_for_runtime(&self, runtime_id: &str) -> Option<String> {
        self.runtime_of
            .iter()
            .find_map(|(stored, rt)| (rt == runtime_id).then(|| stored.clone()))
    }

    /// History reads run off the transport loop; the task emits the same
    /// events with the same redaction as the inline path.
    fn history(&self, stored_id: String, before: usize) {
        tokio::spawn(fetch_history_task(
            self.http.clone(),
            self.origin.clone(),
            self.secrets.clone(),
            self.events.clone(),
            stored_id,
            before,
        ));
    }

    async fn begin_delete(&mut self, stored_id: String) {
        if self.refused.contains(&stored_id) {
            self.request_delete(stored_id, true).await;
            return;
        }
        if let Some(runtime_id) = self.runtime_of.get(&stored_id).cloned() {
            let _ = self
                .send_rpc(
                    "session.interrupt",
                    json!({"session_id": runtime_id}),
                    JobKind::Ping,
                    RPC_TIMEOUT,
                )
                .await;
            if self
                .send_rpc(
                    "session.close",
                    json!({"session_id": runtime_id}),
                    JobKind::Close {
                        stored_id: stored_id.clone(),
                        retried: false,
                    },
                    RPC_TIMEOUT,
                )
                .await
                .is_ok()
            {
                return;
            }
        }
        self.request_delete(stored_id, false).await;
    }

    async fn request_delete(&mut self, stored_id: String, retried: bool) {
        if let Err(fault) = self
            .send_rpc(
                "session.delete",
                json!({"session_id": stored_id}),
                JobKind::Delete {
                    stored_id: stored_id.clone(),
                    retried,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "delete",
                Some(stored_id),
                send_fault_message("delete", fault),
            )
            .await;
        }
    }

    async fn respond(&mut self, runtime_id: String, request_id: String, answer: InteractionAnswer) {
        let Some(ask) = self.asks.get(&request_id) else {
            self.fail(
                "respond",
                Some(runtime_id),
                "That question is no longer open.",
            )
            .await;
            return;
        };
        if ask.runtime_id != runtime_id {
            self.fail(
                "respond",
                Some(runtime_id),
                "This question belongs to another conversation.",
            )
            .await;
            return;
        }
        if self
            .turns
            .get(&runtime_id)
            .is_some_and(|turn| turn.phase == TurnPhase::Stopping)
        {
            let rpc_id = ask.rpc_id.clone();
            self.asks.remove(&request_id);
            self.decline_server_request(&runtime_id, &rpc_id).await;
            return;
        }
        let result = match interaction_result(&ask.method, &ask.params, &answer) {
            Ok(result) => result,
            Err(message) => {
                self.fail("respond", Some(runtime_id), message).await;
                return;
            }
        };
        let rpc_id = ask.rpc_id.clone();
        self.asks.remove(&request_id);
        match self.send_raw(&rpc_result(&rpc_id, result)).await {
            Ok(()) => {
                if let Some(turn) = self.turns.get_mut(&runtime_id) {
                    turn.note_withdraw();
                }
            }
            Err(fault) => {
                self.fail(
                    "respond",
                    Some(runtime_id),
                    send_fault_message("respond", fault),
                )
                .await;
            }
        }
    }

    async fn decline_server_request(&mut self, runtime_id: &str, rpc_id: &Value) {
        match self.send_raw(&rpc_error(rpc_id, -32000, "cancelled")).await {
            Ok(()) => {}
            Err(fault) => {
                self.fail(
                    "interaction",
                    Some(runtime_id.to_string()).filter(|id| !id.is_empty()),
                    send_fault_message("interaction", fault),
                )
                .await;
            }
        }
    }

    async fn send_rpc(
        &mut self,
        method: &str,
        params: Value,
        kind: JobKind,
        timeout: Duration,
    ) -> Result<u64, SendFault> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.jobs.insert(
            id,
            Job {
                kind,
                deadline: Instant::now() + timeout,
            },
        );
        if let Err(fault) = self.send_raw(&frame).await {
            self.jobs.remove(&id);
            return Err(fault);
        }
        Ok(id)
    }

    async fn send_raw(&mut self, value: &Value) -> Result<(), SendFault> {
        let Some(write) = self.write.as_mut() else {
            return Err(SendFault::NotSent);
        };
        let text = value.to_string();
        if matches!(
            tokio::time::timeout(WRITE_TIMEOUT, write.send(Message::text(text))).await,
            Ok(Ok(()))
        ) {
            Ok(())
        } else {
            self.close_socket().await;
            self.emit(Event::Connection(ConnectionState::Disconnected(
                "The connection failed while sending. Nothing already sent will be replayed."
                    .into(),
            )))
            .await;
            Err(SendFault::Ambiguous)
        }
    }

    async fn on_socket(
        &mut self,
        frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) {
        let Some(frame) = frame else {
            self.drop_socket("The gateway socket closed.").await;
            return;
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(_) => {
                self.drop_socket("The gateway socket failed.").await;
                return;
            }
        };
        self.last_inbound = Instant::now();
        // Retry parked streaming tails now that a frame has arrived: the UI
        // had a whole inter-frame gap to drain, so resume latency is one
        // frame instead of the 5s maintenance tick. The tick stays as the
        // fallback when no frames flow (server-side compress).
        self.flush_pending_streams();
        match frame {
            Message::Text(text) => self.on_text(text.as_str()).await,
            Message::Ping(payload) => {
                let failed = if let Some(write) = self.write.as_mut() {
                    !matches!(
                        tokio::time::timeout(WRITE_TIMEOUT, write.send(Message::Pong(payload))).await,
                        Ok(Ok(()))
                    )
                } else {
                    false
                };
                if failed {
                    self.drop_socket("The connection failed while sending.")
                        .await;
                }
            }
            Message::Pong(_) => {}
            Message::Close(_) => self.drop_socket("The gateway socket closed.").await,
            Message::Binary(_) | Message::Frame(_) => {}
        }
    }

    async fn on_text(&mut self, text: &str) {
        // Streaming is the hot path: a `message.delta` frame parses straight
        // into borrowed fields with no JSON DOM. The needle pre-filter keeps
        // every other frame — including big tool outputs and history
        // replies — on its single full parse; a literal needle inside a
        // payload falls through to the envelope check and then the fallback,
        // so routing is unchanged.
        if text.contains("\"message.delta\"")
            && let Some((runtime_id, payload_text, turn_id, segment, offset)) =
                delta_frame_fields(text)
        {
            self.on_delta_fields(runtime_id, payload_text, turn_id, segment, offset)
                .await;
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if value.get("method").and_then(Value::as_str) == Some("event") {
            self.on_event(&value).await;
            return;
        }
        if let Some(method) = value.get("method").and_then(Value::as_str)
            && value.get("id").is_some()
            && value.get("result").is_none()
            && value.get("error").is_none()
        {
            self.on_server_request(method, &value).await;
            return;
        }
        if let Some(id) = value.get("id").and_then(Value::as_u64)
            && let Some(job) = self.jobs.remove(&id)
        {
            self.on_job(job, &value).await;
        }
    }

    async fn on_event(&mut self, value: &Value) {
        let null = Value::Null;
        let params = value.get("params").unwrap_or(&null);
        let event_type = params.get("type").and_then(Value::as_str).unwrap_or("");
        let runtime_id = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let payload = params.get("payload").unwrap_or(&null);
        if event_type == "gateway.ready" {
            self.heartbeat = payload
                .get("heartbeat")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.finish_ready().await;
            return;
        }
        match event_type {
            "message.start" => self.on_message_start(&runtime_id, payload).await,
            "message.delta" => self.on_delta(&runtime_id, payload).await,
            "message.complete" => self.on_complete(&runtime_id, payload).await,
            "thinking.delta"
            | "reasoning.delta"
            | "reasoning.available"
            | "btw.complete"
            | "session.usage"
            | "session.info" => {
                self.on_inspector_event(&runtime_id, event_type, payload)
                    .await
            }
            "tool.start" | "tool.generating" | "tool.progress" | "tool.output_risk"
            | "tool.complete" => self.on_tool_event(&runtime_id, event_type, payload).await,
            "request.cancel" => self.on_request_cancel(payload).await,
            "error" => self.on_gateway_error(runtime_id, payload).await,
            "session.title" => self.on_session_title(&runtime_id, payload).await,
            "sessions.changed" | "platforms.changed" | "message.interim" => {}
            // Named `event` frames are notifications: they need no response.
            // An unknown name is unrelated metadata, not a conversation failure.
            // `error` and failed `message.complete` stay above. A missing type
            // is a malformed frame and is reported without a session.
            other => {
                if matches!(
                    classify_unhandled_event(other),
                    UnhandledGatewayEvent::Malformed
                ) {
                    self.fail(
                        "protocol",
                        None,
                        "The gateway sent an event without a type.",
                    )
                    .await;
                }
            }
        }
    }

    async fn on_inspector_event(&mut self, runtime_id: &str, event_type: &str, payload: &Value) {
        if runtime_id.is_empty() {
            return;
        }
        match event_type {
            "thinking.delta" | "reasoning.delta" => {
                self.activity(runtime_id, "Hermes is thinking…").await;
                self.emit_reasoning_payload(runtime_id, payload, false);
            }
            "reasoning.available" => {
                self.emit_reasoning_payload(runtime_id, payload, true);
            }
            "btw.complete" => self.on_btw_complete(runtime_id, payload).await,
            "session.usage" => {
                if let Some(telemetry) = parse_telemetry_payload(payload) {
                    self.emit(Event::Telemetry {
                        runtime_id: runtime_id.to_string(),
                        telemetry,
                    })
                    .await;
                }
            }
            "session.info" => {
                self.emit_live_info(runtime_id, &json!({"info": payload}))
                    .await
            }
            _ => {}
        }
    }

    /// Reasoning streams are per-token like text deltas, so they park with
    /// the same shape the UI applies live: a replace resets the tail, a
    /// delta appends. Flushed on the next inbound frame or maintenance tick.
    fn emit_reasoning_payload(&mut self, runtime_id: &str, payload: &Value, replace: bool) {
        let Some(raw) = payload.get("text").and_then(Value::as_str) else {
            return;
        };
        if raw.is_empty() {
            return;
        }
        let sent = self.events.try_send(Event::Reasoning {
            runtime_id: runtime_id.to_string(),
            text: redact_secrets(raw, &self.secrets),
            replace,
        });
        if let Err(TrySendError::Full(Event::Reasoning { text, replace, .. })) = sent {
            let entry = self
                .pending_reasoning
                .entry(runtime_id.to_string())
                .or_insert((false, String::new()));
            if replace {
                *entry = (true, text);
            } else {
                entry.1.push_str(&text);
            }
            trim_to_cap(&mut entry.1, DELTA_TAIL_CAP);
        }
    }

    async fn on_btw_complete(&self, runtime_id: &str, payload: &Value) {
        let task_id = payload
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let question = payload
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        self.emit(Event::SideComplete {
            runtime_id: runtime_id.to_string(),
            task_id,
            question: redact_secrets(&question, &self.secrets),
            text: redact_secrets(&text, &self.secrets),
        })
        .await;
    }

    async fn emit_live_info(&self, runtime_id: &str, result: &Value) {
        if runtime_id.is_empty() {
            return;
        }
        if let Some(telemetry) = parse_live_info_telemetry(result) {
            self.emit(Event::Telemetry {
                runtime_id: runtime_id.to_string(),
                telemetry,
            })
            .await;
        }
    }

    async fn on_request_cancel(&mut self, payload: &Value) {
        let Some(raw_id) = payload.get("id").or_else(|| payload.get("request_id")) else {
            return;
        };
        let Some((owner, request_id)) = self.take_ask(raw_id) else {
            return;
        };
        let waiting = self.asks.values().any(|ask| ask.runtime_id == owner);
        if !waiting && let Some(turn) = self.turns.get_mut(&owner) {
            turn.note_withdraw();
        }
        self.emit(Event::InteractionCancelled {
            runtime_id: owner,
            request_id,
        })
        .await;
    }

    async fn on_gateway_error(&mut self, runtime_id: String, payload: &Value) {
        if self
            .turns
            .get(&runtime_id)
            .is_some_and(|turn| turn.phase == TurnPhase::Terminal)
        {
            return;
        }
        if let Some(turn) = self.turns.get_mut(&runtime_id) {
            let _ = turn.finish();
        }
        self.retire_turn_jobs(&runtime_id);
        let message = payload
            .get("message")
            .or_else(|| payload.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("The gateway reported an error.");
        self.fail(
            "gateway",
            Some(runtime_id).filter(|s| !s.is_empty()),
            message,
        )
        .await;
    }

    async fn on_session_title(&mut self, runtime_id: &str, payload: &Value) {
        let Some(raw_title) = payload
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let session_id = self.resolve_title_session_id(runtime_id, payload);
        if session_id.is_empty() {
            return;
        }
        self.emit(Event::SessionTitle {
            session_id,
            title: self.clean(raw_title.to_string()),
        })
        .await;
    }

    fn resolve_title_session_id(&self, runtime_id: &str, payload: &Value) -> String {
        if let Some(stored) = payload
            .get("stored_session_id")
            .or_else(|| payload.get("session_key"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return stored.to_string();
        }
        if let Some(stored) = self
            .runtime_of
            .iter()
            .find_map(|(stored, rt)| (rt == runtime_id).then(|| stored.clone()))
        {
            return stored;
        }
        if !runtime_id.is_empty() {
            return runtime_id.to_string();
        }
        payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Turn-rollover handling for message.start, in the documented order:
    /// ensure_turn first, then the rollover notice, then submit-job retirement.
    async fn on_message_start(&mut self, runtime_id: &str, payload: &Value) {
        let message_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("turn-{runtime_id}"));
        let (turn, server_started) = self.ensure_turn(runtime_id, &message_id);
        let accepted = turn.note_accepted();
        if server_started {
            self.emit(Event::TurnRolled {
                runtime_id: runtime_id.to_string(),
            })
            .await;
        }
        if accepted {
            self.retire_submit_jobs(runtime_id);
        }
    }

    async fn on_tool_event(&mut self, runtime_id: &str, event_type: &str, payload: &Value) {
        let name = tool_name_from(payload);
        let label = match event_type {
            "tool.complete" => format!("Tool finished: {}", truncate(name, 80)),
            "tool.output_risk" => format!("Tool flagged output: {}", truncate(name, 80)),
            _ => format!("Using tool: {}", truncate(name, 80)),
        };
        self.activity(runtime_id, &label).await;
        if !runtime_id.is_empty()
            && let Some(update) = parse_tool_event(event_type, payload, &self.secrets)
        {
            self.emit(Event::ToolTraceEvent {
                runtime_id: runtime_id.to_string(),
                update,
            })
            .await;
        }
    }

    async fn on_delta(&mut self, runtime_id: &str, payload: &Value) {
        let Some(text) = payload.get("text").and_then(Value::as_str) else {
            return;
        };
        let turn_id = payload.get("turn_id").and_then(Value::as_str).unwrap_or("");
        let segment = payload.get("segment").and_then(Value::as_i64).unwrap_or(0);
        let offset = payload.get("offset").and_then(Value::as_i64).unwrap_or(0);
        self.on_delta_fields(runtime_id, text, turn_id, segment, offset)
            .await;
    }

    /// The delta state machine, shared by the borrowed fast path and the
    /// DOM fallback: accept-ordering against the turn, segment rollover,
    /// then the non-blocking emit.
    async fn on_delta_fields(
        &mut self,
        runtime_id: &str,
        text: &str,
        turn_id: &str,
        segment: i64,
        offset: i64,
    ) {
        if text.is_empty() {
            return;
        }
        let (turn, server_started) = self.ensure_turn(runtime_id, turn_id);
        let accepted = turn.note_accepted();
        if !accepted {
            return;
        }
        if segment != turn.segment {
            turn.segment = segment;
            turn.applied_offset = 0;
        }
        if offset < turn.applied_offset {
            return;
        }
        turn.applied_offset = offset.saturating_add(utf16_len(text) as i64);
        let message_id = turn.message_id.clone();
        self.retire_submit_jobs(runtime_id);
        if server_started {
            self.emit(Event::TurnRolled {
                runtime_id: runtime_id.to_string(),
            })
            .await;
        }
        self.emit_delta(runtime_id, &message_id, text).await;
    }

    /// Delta events ride a non-blocking send with an in-memory tail: a slow UI
    /// must never stall the transport loop, which has to keep answering pings
    /// and draining commands. A tail is capped; `MessageComplete` carries the
    /// authoritative text, so trimming is a display-only degradation.
    ///
    /// The pending entry survives successful sends: ids ride `Arc<str>`
    /// clones and the map key is reused, so a steady frame allocates only
    /// the delta buffer (a cold insert costs the key plus two id `Arc`s
    /// once per runtime). `MessageComplete` and `close_socket` clear it.
    async fn emit_delta(&mut self, runtime_id: &str, message_id: &str, delta: &str) {
        let entry = match self.pending_deltas.get_mut(runtime_id) {
            Some(entry) => entry,
            None => {
                self.pending_deltas.insert(
                    runtime_id.to_string(),
                    PendingDelta {
                        runtime: Arc::from(runtime_id),
                        message: Arc::from(message_id),
                        tail: String::new(),
                    },
                );
                self.pending_deltas
                    .get_mut(runtime_id)
                    .expect("entry inserted above")
            }
        };
        if entry.message.as_ref() != message_id {
            // The UI's streaming row moved to a new message. Try to hand over
            // the old tail first; if the channel is full the old tail is
            // dropped rather than concatenated out of order. An empty tail
            // (the previous frame was already sent) needs no event.
            let old_message = std::mem::replace(&mut entry.message, Arc::from(message_id));
            if !entry.tail.is_empty() {
                let event = Event::MessageDelta {
                    runtime_id: entry.runtime.clone(),
                    message_id: old_message,
                    delta: std::mem::take(&mut entry.tail),
                };
                let _ = self.events.try_send(event);
            }
        }
        entry.tail.push_str(delta);
        trim_to_cap(&mut entry.tail, DELTA_TAIL_CAP);
        let sent = self.events.try_send(Event::MessageDelta {
            runtime_id: entry.runtime.clone(),
            message_id: entry.message.clone(),
            delta: std::mem::take(&mut entry.tail),
        });
        match sent {
            Ok(()) => {}
            // Park the tail for the next delta or the next maintenance tick.
            Err(TrySendError::Full(Event::MessageDelta { delta, .. })) => {
                entry.tail = delta;
            }
            Err(TrySendError::Closed(Event::MessageDelta { delta, .. })) => {
                entry.tail = delta;
            }
            // The event we sent is always a MessageDelta; other payloads in
            // the error are statically impossible but keep the match total.
            Err(_) => {}
        }
    }

    /// Hand a parked streaming tail to the UI if it has room now. An entry
    /// with an empty tail lingers after a successful send; flushing it would
    /// emit a pointless empty delta, so it reports no room instead.
    fn try_flush_delta(&mut self, runtime_id: &str) -> bool {
        let Some(entry) = self.pending_deltas.get_mut(runtime_id) else {
            return false;
        };
        if entry.tail.is_empty() {
            return false;
        }
        let event = Event::MessageDelta {
            runtime_id: entry.runtime.clone(),
            message_id: entry.message.clone(),
            delta: std::mem::take(&mut entry.tail),
        };
        match self.events.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(Event::MessageDelta { delta, .. })) => {
                entry.tail = delta;
                false
            }
            Err(TrySendError::Closed(Event::MessageDelta { delta, .. })) => {
                entry.tail = delta;
                false
            }
            Err(_) => false,
        }
    }

    /// Hand a parked reasoning tail to the UI if it has room now, replaying
    /// the replace flag the live stream would have carried.
    fn try_flush_reasoning(&mut self, runtime_id: &str) -> bool {
        let Some((replace, tail)) = self.pending_reasoning.get(runtime_id) else {
            return false;
        };
        let event = Event::Reasoning {
            runtime_id: runtime_id.to_string(),
            text: tail.clone(),
            replace: *replace,
        };
        if self.events.try_send(event).is_ok() {
            self.pending_reasoning.remove(runtime_id);
            true
        } else {
            false
        }
    }

    async fn on_complete(&mut self, runtime_id: &str, payload: &Value) {
        if self
            .turns
            .get(runtime_id)
            .is_some_and(|turn| turn.phase == TurnPhase::Terminal)
        {
            return;
        }
        let status = turn_status(payload.get("status").and_then(Value::as_str).unwrap_or(""));
        let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
        let (turn, _) = self.ensure_turn(runtime_id, "");
        let message_id = turn.message_id.clone();
        if let Some(turn) = self.turns.get_mut(runtime_id) {
            let _ = turn.finish();
        }
        self.retire_turn_jobs(runtime_id);
        self.activity.remove(runtime_id);
        // The authoritative completion supersedes any parked streaming tail.
        self.pending_deltas.remove(runtime_id);
        self.emit(Event::MessageComplete {
            runtime_id: runtime_id.to_string(),
            message: ChatMessage {
                id: message_id.to_string(),
                role: Role::Assistant,
                text: text.to_owned(),
            },
            status,
        })
        .await;
        self.flush_deferred_for_runtime(runtime_id);
        self.emit_complete_metadata(runtime_id, payload).await;
    }

    async fn emit_complete_metadata(&self, runtime_id: &str, payload: &Value) {
        if runtime_id.is_empty() {
            return;
        }
        // The completion's usage names the model this inference actually ran on. That is
        // the only direct evidence of an effective model, so it reconciles the display.
        if let Some(model) = payload
            .get("usage")
            .and_then(|usage| usage.get("model"))
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
        {
            self.emit(Event::ModelObserved {
                runtime_id: runtime_id.to_string(),
                model: model.to_string(),
            })
            .await;
        }
        if let Some(reasoning) = payload
            .get("reasoning")
            .and_then(Value::as_str)
            .filter(|r| !r.trim().is_empty())
        {
            self.emit(Event::Reasoning {
                runtime_id: runtime_id.to_string(),
                text: redact_secrets(reasoning, &self.secrets),
                replace: true,
            })
            .await;
        }
        if let Some(telemetry) = parse_telemetry_payload(payload) {
            self.emit(Event::Telemetry {
                runtime_id: runtime_id.to_string(),
                telemetry,
            })
            .await;
        }
    }

    fn ensure_turn(&mut self, runtime_id: &str, message_id: &str) -> (&mut Turn, bool) {
        let mut server_started = false;
        if !self.turns.contains_key(runtime_id) {
            self.next_turn_generation += 1;
            let id = if message_id.is_empty() {
                format!("turn-{runtime_id}-{}", self.next_turn_generation)
            } else {
                message_id.to_string()
            };
            self.turns.insert(
                runtime_id.to_string(),
                Turn::streaming(id.into(), self.next_turn_generation),
            );
            // A turn nobody tracked just started: a server-initiated message
            // (a queued follow-up that survived a reconnect), never a client
            // submit. The UI must hear about it to leave its settled state.
            server_started = !message_id.is_empty();
        } else {
            // A server-initiated follow-up (FIFO-drained queued prompt) starts
            // a new message after the live turn settled with no client submit.
            // Roll the tracking to the new message; the fresh generation keeps
            // late frames of the old turn from affecting it.
            let rolls = {
                let turn = &self.turns[runtime_id];
                turn.phase == TurnPhase::Terminal
                    && !message_id.is_empty()
                    && turn.message_id.as_ref() != message_id
            };
            if rolls {
                self.next_turn_generation += 1;
                let generation = self.next_turn_generation;
                // A stale reasoning tail belongs to the finished turn; the
                // new turn's trace starts clean.
                self.pending_reasoning.remove(runtime_id);
                self.turns.insert(
                    runtime_id.to_string(),
                    Turn::streaming(Arc::from(message_id), generation),
                );
                self.retire_turn_jobs(runtime_id);
                server_started = true;
            }
        }
        let turn = self.turns.get_mut(runtime_id).expect("inserted turn");
        if !message_id.is_empty()
            && turn.phase != TurnPhase::Terminal
            && turn.message_id.as_ref() != message_id
        {
            turn.message_id = message_id.into();
        }
        (turn, server_started)
    }

    fn retire_submit_jobs(&mut self, runtime_id: &str) {
        self.jobs.retain(|_, job| {
            !matches!(
                &job.kind,
                JobKind::Submit { runtime_id: id, .. }
                    | JobKind::AttachImage { runtime_id: id, .. }
                    if id == runtime_id
            )
        });
    }

    fn retire_turn_jobs(&mut self, runtime_id: &str) {
        self.jobs.retain(|_, job| {
            !matches!(
                &job.kind,
                JobKind::Submit { runtime_id: id, .. }
                    | JobKind::AttachImage { runtime_id: id, .. }
                    | JobKind::Interrupt { runtime_id: id, .. }
                    if id == runtime_id
            )
        });
    }

    fn take_ask(&mut self, id: &Value) -> Option<(String, String)> {
        if let Some(key) = request_key(id)
            && let Some(ask) = self.asks.remove(&key)
        {
            return Some((ask.runtime_id, key));
        }
        let found = self
            .asks
            .iter()
            .find_map(|(key, ask)| same_request_id(&ask.rpc_id, id).then(|| key.clone()));
        found.and_then(|key| self.asks.remove(&key).map(|ask| (ask.runtime_id, key)))
    }

    async fn activity(&mut self, runtime_id: &str, text: &str) {
        if runtime_id.is_empty()
            || self
                .turns
                .get(runtime_id)
                .is_some_and(|turn| matches!(turn.phase, TurnPhase::Terminal | TurnPhase::Stopping))
            || self.activity.get(runtime_id).is_some_and(|old| old == text)
        {
            return;
        }
        self.activity
            .insert(runtime_id.to_string(), text.to_string());
        self.emit(Event::Activity {
            runtime_id: runtime_id.to_string(),
            text: text.to_string(),
        })
        .await;
    }

    async fn on_server_request(&mut self, method: &str, value: &Value) {
        let Some(rpc_id) = value
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .cloned()
        else {
            return;
        };
        let Some(id) = request_key(&rpc_id) else {
            return;
        };
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let runtime_id = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if method != "approval" && method != "clarify" {
            let _ = self
                .send_raw(&rpc_error(&rpc_id, -32601, "method not supported"))
                .await;
            self.fail(
                "interaction",
                Some(runtime_id).filter(|s| !s.is_empty()),
                format!("The server asked for {method}, which this client does not answer."),
            )
            .await;
            return;
        }
        if self
            .turns
            .get(&runtime_id)
            .is_some_and(|turn| turn.phase == TurnPhase::Stopping)
        {
            self.decline_server_request(&runtime_id, &rpc_id).await;
            return;
        }
        let (kind, prompt, choices, questions, answers) = if method == "approval" {
            (
                InteractionKind::Approval,
                approval_prompt(&params),
                approval_choices(&params),
                Vec::new(),
                HashMap::new(),
            )
        } else {
            let questions = match clarification_questions(&params) {
                Ok(questions) => questions,
                Err(message) => {
                    let _ = self.send_raw(&rpc_error(&rpc_id, -32602, &message)).await;
                    self.fail("interaction", Some(runtime_id), message).await;
                    return;
                }
            };
            let answers = match locked_answers(&params) {
                Ok(answers) => answers,
                Err(message) => {
                    let _ = self.send_raw(&rpc_error(&rpc_id, -32602, &message)).await;
                    self.fail("interaction", Some(runtime_id), message).await;
                    return;
                }
            };
            (
                InteractionKind::Clarification,
                "Hermes needs your input.".into(),
                Vec::new(),
                questions,
                answers,
            )
        };
        if !runtime_id.is_empty() {
            self.retire_submit_jobs(&runtime_id);
            let (turn, _) = self.ensure_turn(&runtime_id, "");
            turn.await_input();
        }
        self.asks.insert(
            id.clone(),
            Ask {
                method: method.to_string(),
                params,
                runtime_id: runtime_id.clone(),
                rpc_id,
            },
        );
        self.emit(Event::Interaction {
            runtime_id,
            request: InteractionRequest {
                id,
                kind,
                prompt,
                choices,
                questions,
                answers,
            },
        })
        .await;
    }

    async fn on_job(&mut self, job: Job, value: &Value) {
        if let Some(error) = value.get("error") {
            self.on_rpc_error(job, error).await;
            return;
        }
        let null = Value::Null;
        let result = value.get("result").unwrap_or(&null);
        match job.kind {
            JobKind::Capabilities | JobKind::Ping => {}
            JobKind::Close { stored_id, retried } => self.request_delete(stored_id, retried).await,
            JobKind::Create { title } => self.on_created(title, result).await,
            JobKind::Resume { stored_id } => self.on_resumed(stored_id, result).await,
            JobKind::Submit {
                runtime_id,
                generation,
            } => self.on_submit_reply(runtime_id, generation).await,
            JobKind::AttachImage {
                runtime_id,
                generation,
                remaining_paths,
                prompt_text,
            } => {
                self.advance_attach_or_submit(runtime_id, generation, remaining_paths, prompt_text)
                    .await;
            }
            JobKind::Interrupt {
                runtime_id,
                generation,
            } => self.on_interrupt_reply(runtime_id, generation).await,
            JobKind::Steer { runtime_id, text } => {
                self.on_steer_reply(runtime_id, text, result).await
            }
            JobKind::Queue { runtime_id } => {
                self.emit(Event::SteerAccepted {
                    runtime_id,
                    outcome: SteerOutcome::Queued,
                })
                .await;
            }
            JobKind::Catalog => self.on_catalog_reply(result).await,
            JobKind::Prime {
                runtime_id,
                target,
                confirm_expensive,
            } => {
                // The agent build the prime waited for now exists, so the scoped switch
                // applies to it. A failed prime means no switch was attempted.
                self.begin_switch(runtime_id, target, confirm_expensive)
                    .await;
            }
            JobKind::Switch {
                runtime_id, target, ..
            } => self.model_reply(runtime_id, target, result).await,
            JobKind::SideQuestion {
                runtime_id,
                client_id,
            } => self.on_side_reply(runtime_id, client_id, result).await,
            JobKind::Delete { stored_id, .. } => {
                self.runtime_of.remove(&stored_id);
                self.pinned_sessions.remove(&stored_id);
                self.deferred_mutations.remove(&stored_id);
                self.emit(Event::Deleted { stored_id }).await;
            }
            JobKind::DeleteResume { stored_id } => {
                self.on_delete_resume_reply(stored_id, result).await
            }
            kind => self.on_timeline_job(kind, result).await,
        }
    }

    async fn on_timeline_job(&mut self, kind: JobKind, result: &Value) {
        match kind {
            JobKind::Branch {
                parent_stored_id,
                parent_title,
                fallback_title,
                fallback_seed,
            } => {
                self.on_live_branch_reply(
                    parent_stored_id,
                    parent_title,
                    fallback_title,
                    fallback_seed.len(),
                    result,
                )
                .await;
            }
            JobKind::BranchClose(pending) => self.reopen_branched(pending).await,
            JobKind::BranchResume(pending) => {
                self.finish_reopened_branch(pending, result).await;
            }
            JobKind::BranchStored {
                parent_stored_id,
                parent_title,
                fallback_title,
                fallback_seed,
            } => {
                self.on_branched(
                    parent_stored_id,
                    parent_title,
                    fallback_title,
                    fallback_seed.len(),
                    result,
                )
                .await;
            }
            JobKind::BranchSeeded {
                parent_stored_id,
                parent_title,
                title,
                seed_count,
            } => {
                self.on_branched(parent_stored_id, parent_title, title, seed_count, result)
                    .await;
            }
            JobKind::Undo {
                stored_id,
                runtime_id,
                mode,
                prompt_text,
                ..
            } => {
                self.on_undo_reply(stored_id, runtime_id, mode, prompt_text, result)
                    .await;
            }
            JobKind::UndoSync {
                stored_id,
                runtime_id,
                removed,
                mode,
                prompt_text,
            } => {
                self.on_undo_sync_reply(stored_id, runtime_id, removed, mode, prompt_text, result)
                    .await;
            }
            other => self.on_curation_job(other, result).await,
        }
    }

    async fn on_curation_job(&mut self, kind: JobKind, result: &Value) {
        match kind {
            JobKind::ArchiveClose { stored_id, retried } => {
                self.request_archive_patch(stored_id, retried).await;
            }
            JobKind::ArchiveResume { stored_id } => {
                if let Some(runtime_id) = result
                    .get("session_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    && self
                        .close_runtime_for_archive(&stored_id, runtime_id, true)
                        .await
                {
                    return;
                }
                self.request_archive_patch(stored_id, true).await;
            }
            JobKind::Compress {
                stored_id,
                runtime_id,
            } => {
                self.on_compress_reply(stored_id, runtime_id, result).await;
            }
            JobKind::CompressSync {
                stored_id,
                runtime_id,
                summary,
            } => {
                self.on_compress_sync_reply(stored_id, runtime_id, summary, result)
                    .await;
            }
            JobKind::SessionInfo { runtime_id } => {
                self.emit_live_info(&runtime_id, result).await;
            }
            _ => {}
        }
    }

    async fn on_compress_reply(&mut self, stored_id: String, runtime_id: String, result: &Value) {
        self.emit_live_info(&runtime_id, result).await;
        if let Some(telemetry) = parse_telemetry_payload(result) {
            self.emit(Event::Telemetry {
                runtime_id: runtime_id.clone(),
                telemetry,
            })
            .await;
        }
        let creds = self.credential_secrets();
        let summary = studio::parse_compress_summary(result, &creds);
        if summary.noop {
            self.emit(Event::Compressed {
                stored_id,
                runtime_id,
                messages: None,
                before: 0,
                summary,
            })
            .await;
            return;
        }
        if let Err(fault) = self
            .send_rpc(
                "session.resume",
                json!({
                    "session_id": stored_id,
                    "inline_images": false,
                    "omit_messages": true
                }),
                JobKind::CompressSync {
                    stored_id: stored_id.clone(),
                    runtime_id,
                    summary,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "compress",
                Some(stored_id),
                send_fault_message("compress", fault),
            )
            .await;
        }
    }

    async fn on_compress_sync_reply(
        &mut self,
        stored_id: String,
        runtime_id: String,
        summary: CompressSummary,
        result: &Value,
    ) {
        let effective_runtime = result
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(runtime_id);
        self.runtime_of
            .insert(stored_id.clone(), effective_runtime.clone());
        let count = result
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        self.message_counts.insert(stored_id.clone(), count);
        let before = count.saturating_sub(PAGE);
        // The post-sync page loads off the transport loop; the completion
        // event applies when the rows return.
        self.load_synced_page(
            stored_id,
            before,
            count - before,
            PageAction::Compress {
                runtime_id: effective_runtime,
                summary,
            },
        );
    }

    /// Spawns the off-loop page fetch for a sync reply, carrying the action
    /// the rows complete.
    fn load_synced_page(&self, stored_id: String, before: usize, limit: usize, action: PageAction) {
        tokio::spawn(synced_page_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            SyncedPage {
                stored_id,
                before,
                // The task overwrites this with the fetched rows.
                messages: Ok(Vec::new()),
                action,
            },
            limit,
        ));
    }

    /// Applies a fetched sync page on the loop: the undo or compress
    /// completion event, or the same per-operation failure the inline path
    /// produced.
    async fn apply_synced_page(&mut self, page: SyncedPage) {
        let SyncedPage {
            stored_id,
            before,
            messages,
            action,
        } = page;
        let operation = match &action {
            PageAction::Undo { .. } => "undo",
            PageAction::Compress { .. } => "compress",
        };
        let messages = match messages {
            Ok(rows) => rows,
            Err(err) => {
                self.fail(operation, Some(stored_id), err).await;
                return;
            }
        };
        match action {
            PageAction::Undo {
                runtime_id,
                removed,
                mode,
                prompt_text,
            } => {
                self.emit(Event::Undone {
                    stored_id,
                    runtime_id,
                    removed,
                    messages,
                    before,
                    mode,
                    prompt_text,
                })
                .await;
            }
            PageAction::Compress {
                runtime_id,
                summary,
            } => {
                self.emit(Event::Compressed {
                    stored_id,
                    runtime_id,
                    messages: Some(messages),
                    before,
                    summary,
                })
                .await;
            }
        }
    }

    fn build_pending_branch(
        &self,
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_count: usize,
        result: &Value,
    ) -> Option<PendingBranch> {
        let (temp_runtime_id, stored_id) = extract_session_ids(result)?;
        let count = result
            .get("message_count")
            .and_then(Value::as_u64)
            .map_or(fallback_count, |n| n as usize);
        let raw_title = payload_str(result, &["title"]);
        let title = if raw_title.is_empty() {
            fallback_title
        } else {
            self.clean(raw_title)
        };
        let fallback_messages = result
            .get("messages")
            .and_then(Value::as_array)
            .map(|rows| messages_from(rows))
            .unwrap_or_default();
        Some(PendingBranch {
            stored_id,
            temp_runtime_id,
            parent_stored_id,
            parent_title,
            title,
            count,
            fallback_messages,
            model: model_identity(result),
        })
    }

    /// Hermes v0.23.0 `session.branch` leaves `branch_db = None` on the child's initial
    /// runtime in the default profile (`methods_session_branch.py:33`), while the branch row
    /// and copied messages are already persisted in SQLite. Closing that transient runtime and
    /// immediately resuming `stored_id` binds a full DB-backed runtime so subsequent turns and
    /// `session.undo` rewinds persist cleanly.
    async fn on_live_branch_reply(
        &mut self,
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_count: usize,
        result: &Value,
    ) {
        let Some(pending) = self.build_pending_branch(
            parent_stored_id.clone(),
            parent_title,
            fallback_title,
            fallback_count,
            result,
        ) else {
            self.fail(
                "branch",
                Some(parent_stored_id),
                "The gateway did not return a branched conversation id.",
            )
            .await;
            return;
        };
        let temp_rt = pending.temp_runtime_id.clone();
        let parent_id = pending.parent_stored_id.clone();
        if let Err(fault) = self
            .send_rpc(
                "session.close",
                json!({"session_id": temp_rt}),
                JobKind::BranchClose(pending),
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "branch",
                Some(parent_id),
                send_fault_message("branch", fault),
            )
            .await;
        }
    }

    async fn reopen_branched(&mut self, pending: PendingBranch) {
        let stored_id = pending.stored_id.clone();
        let parent_id = pending.parent_stored_id.clone();
        if let Err(fault) = self
            .send_rpc(
                "session.resume",
                json!({
                    "session_id": stored_id,
                    "inline_images": false,
                    "omit_messages": true
                }),
                JobKind::BranchResume(pending),
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail(
                "branch",
                Some(parent_id),
                send_fault_message("branch", fault),
            )
            .await;
        }
    }

    async fn finish_reopened_branch(&mut self, pending: PendingBranch, result: &Value) {
        let runtime_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| pending.temp_runtime_id.clone());
        let count = result
            .get("message_count")
            .and_then(Value::as_u64)
            .map_or(pending.count, |n| n as usize);
        let model = model_identity(result).or(pending.model.clone());
        self.emit_branched_session(
            pending.stored_id,
            runtime_id.clone(),
            pending.parent_stored_id,
            pending.parent_title,
            pending.title,
            count,
            pending.fallback_messages,
            model,
        )
        .await;
        self.emit_live_info(&runtime_id, result).await;
        self.request_session_info_if_needed(&runtime_id, result)
            .await;
    }

    async fn on_branched(
        &mut self,
        parent_stored_id: String,
        parent_title: String,
        fallback_title: String,
        fallback_count: usize,
        result: &Value,
    ) {
        let Some(pending) = self.build_pending_branch(
            parent_stored_id.clone(),
            parent_title,
            fallback_title,
            fallback_count,
            result,
        ) else {
            self.fail(
                "branch",
                Some(parent_stored_id),
                "The gateway did not return a branched conversation id.",
            )
            .await;
            return;
        };
        let runtime_id = pending.temp_runtime_id.clone();
        self.emit_branched_session(
            pending.stored_id,
            runtime_id.clone(),
            pending.parent_stored_id,
            pending.parent_title,
            pending.title,
            pending.count,
            pending.fallback_messages,
            pending.model,
        )
        .await;
        self.emit_live_info(&runtime_id, result).await;
        self.request_session_info_if_needed(&runtime_id, result)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn emit_branched_session(
        &mut self,
        stored_id: String,
        runtime_id: String,
        parent_stored_id: String,
        parent_title: String,
        title: String,
        count: usize,
        fallback_messages: Vec<ChatMessage>,
        model: Option<ModelIdentity>,
    ) {
        self.runtime_of
            .insert(stored_id.clone(), runtime_id.clone());
        self.message_counts.insert(stored_id.clone(), count);
        let before = count.saturating_sub(PAGE);
        // The branched conversation's first page loads off the transport
        // loop; on error or an empty page the fallback seed rides instead.
        tokio::spawn(branched_page_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            BranchedPage {
                stored_id,
                runtime_id,
                parent_stored_id,
                parent_title,
                title,
                fallback_messages,
                model,
                before,
                // The task overwrites this with the fetched rows.
                messages: Vec::new(),
            },
            count - before,
        ));
    }

    /// Applies a fetched branched page on the loop: the same `Branched`
    /// event the inline path emitted, with the fetched rows or the fallback.
    async fn apply_branched(&mut self, page: BranchedPage) {
        let BranchedPage {
            stored_id,
            runtime_id,
            parent_stored_id,
            parent_title,
            title,
            fallback_messages: _,
            model,
            before,
            messages,
        } = page;
        let model_name = model
            .as_ref()
            .map_or_else(String::new, |entry| entry.model.clone());
        self.emit(Event::Branched {
            session: SessionSummary {
                stored_id,
                title,
                source: "tui".into(),
                updated_at: crate::palette::now_epoch_secs().to_string(),
                read_only_reason: None,
                message_count: before + messages.len(),
                parent_session_id: Some(parent_stored_id),
                is_branch: true,
                pinned: false,
                preview: String::new(),
                model: model_name,
            },
            parent_title,
            runtime_id,
            messages,
            before,
            model,
        })
        .await;
    }

    async fn on_undo_reply(
        &mut self,
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
        result: &Value,
    ) {
        let removed = result.get("removed").and_then(Value::as_u64).unwrap_or(0) as usize;
        if removed == 0 {
            self.fail(
                "undo",
                Some(stored_id),
                "Nothing to rewind in this conversation.",
            )
            .await;
            return;
        }
        if let Err(fault) = self
            .send_rpc(
                "session.resume",
                json!({
                    "session_id": stored_id,
                    "inline_images": false,
                    "omit_messages": true
                }),
                JobKind::UndoSync {
                    stored_id: stored_id.clone(),
                    runtime_id,
                    removed,
                    mode,
                    prompt_text,
                },
                RPC_TIMEOUT,
            )
            .await
        {
            self.fail("undo", Some(stored_id), send_fault_message("undo", fault))
                .await;
        }
    }

    async fn on_undo_sync_reply(
        &mut self,
        stored_id: String,
        runtime_id: String,
        removed: usize,
        mode: RewindMode,
        prompt_text: String,
        result: &Value,
    ) {
        let effective_runtime = result
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(runtime_id);
        self.runtime_of
            .insert(stored_id.clone(), effective_runtime.clone());
        let count = result
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        self.message_counts.insert(stored_id.clone(), count);
        let before = count.saturating_sub(PAGE);
        // The post-sync page loads off the transport loop; the completion
        // event applies when the rows return.
        self.load_synced_page(
            stored_id,
            before,
            count - before,
            PageAction::Undo {
                runtime_id: effective_runtime,
                removed,
                mode,
                prompt_text,
            },
        );
    }

    async fn on_submit_reply(&mut self, runtime_id: String, generation: u64) {
        if self
            .turns
            .get_mut(&runtime_id)
            .is_some_and(|turn| turn.acknowledge(generation))
        {
            self.emit(Event::Submitted { runtime_id }).await;
        }
    }

    async fn on_interrupt_reply(&mut self, runtime_id: String, generation: u64) {
        let accepted = self
            .turns
            .get_mut(&runtime_id)
            .is_some_and(|turn| turn.accept_interrupt(generation));
        if accepted {
            self.emit(Event::Interrupting { runtime_id }).await;
        }
    }

    async fn on_steer_reply(&mut self, runtime_id: String, text: String, result: &Value) {
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("rejected");
        match status {
            "redirected" => {
                self.emit(Event::SteerAccepted {
                    runtime_id,
                    outcome: SteerOutcome::Redirected,
                })
                .await;
            }
            "queued" => {
                self.emit(Event::SteerAccepted {
                    runtime_id,
                    outcome: SteerOutcome::Queued,
                })
                .await;
            }
            // Not running or inapplicable: queue as the next prompt,
            // mirroring the upstream TUI fallback.
            _ => {
                if let Err(fault) = self
                    .send_rpc(
                        "prompt.submit",
                        queued_submit_params(&runtime_id, &text),
                        JobKind::Queue {
                            runtime_id: runtime_id.clone(),
                        },
                        RPC_TIMEOUT,
                    )
                    .await
                {
                    self.fail(
                        "steer",
                        Some(runtime_id),
                        send_fault_message("steer", fault),
                    )
                    .await;
                }
            }
        }
    }

    async fn on_catalog_reply(&self, result: &Value) {
        match Catalog::parse(result) {
            Ok(catalog) => self.emit(Event::Catalog { catalog }).await,
            Err(error) => {
                self.emit(Event::Catalog {
                    catalog: Catalog {
                        error: Some(self.clean(error)),
                        ..Default::default()
                    },
                })
                .await
            }
        }
    }

    async fn on_side_reply(&self, runtime_id: String, client_id: u64, result: &Value) {
        if let Some(task_id) = result
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.emit(Event::SideStarted {
                runtime_id,
                client_id,
                task_id: task_id.to_string(),
            })
            .await;
        }
    }

    async fn on_delete_resume_reply(&mut self, stored_id: String, result: &Value) {
        if let Some(runtime_id) = result
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.runtime_of
                .insert(stored_id.clone(), runtime_id.to_string());
            let _ = self
                .send_rpc(
                    "session.interrupt",
                    json!({"session_id": runtime_id}),
                    JobKind::Ping,
                    RPC_TIMEOUT,
                )
                .await;
            if self
                .send_rpc(
                    "session.close",
                    json!({"session_id": runtime_id}),
                    JobKind::Close {
                        stored_id: stored_id.clone(),
                        retried: true,
                    },
                    RPC_TIMEOUT,
                )
                .await
                .is_ok()
            {
                return;
            }
        }
        self.request_delete(stored_id, true).await;
    }

    async fn fail_model_request(&mut self, kind: JobKind, code: i64, message: &str) {
        match kind {
            JobKind::Catalog => {
                self.emit(Event::Catalog {
                    catalog: Catalog {
                        error: Some(self.clean(format!("The model catalog failed: {message}"))),
                        ..Default::default()
                    },
                })
                .await;
            }
            JobKind::Prime { runtime_id, .. } => {
                self.emit(Event::ModelRejected {
                    runtime_id,
                    message: self.clean(message.to_string()),
                })
                .await;
            }
            JobKind::Switch { runtime_id, .. } => self.fail_switch(runtime_id, code, message).await,
            _ => {}
        }
    }

    async fn fail_switch(&mut self, runtime_id: String, code: i64, message: &str) {
        if code == -32000 {
            self.emit(Event::ModelAmbiguous { runtime_id }).await;
            return;
        }
        self.emit(Event::ModelRejected {
            runtime_id,
            message: self.clean(message.to_string()),
        })
        .await;
    }

    async fn on_rpc_error(&mut self, job: Job, error: &Value) {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("The gateway rejected the request.");
        match job.kind {
            JobKind::Capabilities | JobKind::Ping => {}
            JobKind::Create { .. } => self.fail("create", None, message).await,
            JobKind::Resume { stored_id } => self.resume_refused(stored_id, code, message).await,
            JobKind::Submit {
                runtime_id,
                generation,
            }
            | JobKind::AttachImage {
                runtime_id,
                generation,
                ..
            } => self.fail_submit_job(runtime_id, generation, message).await,
            JobKind::Interrupt {
                runtime_id,
                generation,
            } => {
                self.fail_interrupt_job(runtime_id, generation, message)
                    .await
            }
            // A failed steer must not end the live turn: "steer" is not
            // "submit" in `operation_failed`, so the status stays busy.
            JobKind::Steer { runtime_id, .. } | JobKind::Queue { runtime_id, .. } => {
                self.fail("steer", Some(runtime_id), message).await
            }
            JobKind::SideQuestion {
                runtime_id,
                client_id,
            } => {
                self.fail_side_job(runtime_id, client_id, code, message)
                    .await
            }
            kind @ (JobKind::Catalog | JobKind::Prime { .. } | JobKind::Switch { .. }) => {
                self.fail_model_request(kind, code, message).await;
            }
            kind @ (JobKind::Close { .. }
            | JobKind::DeleteResume { .. }
            | JobKind::Delete { .. }) => {
                self.fail_delete_job(kind, code, message).await;
            }
            kind => self.fail_timeline_job(kind, code, message).await,
        }
    }

    async fn fail_timeline_job(&mut self, kind: JobKind, code: i64, message: &str) {
        match kind {
            JobKind::Undo {
                stored_id,
                runtime_id,
                mode,
                prompt_text,
                attempts,
            } => {
                self.fail_undo_job(
                    stored_id,
                    runtime_id,
                    mode,
                    prompt_text,
                    attempts,
                    code,
                    message,
                )
                .await;
            }
            JobKind::UndoSync {
                stored_id,
                runtime_id,
                removed,
                mode,
                prompt_text,
            } => {
                if code == 4007 {
                    self.message_counts.insert(stored_id.clone(), 0);
                    self.emit(Event::Undone {
                        stored_id,
                        runtime_id,
                        removed,
                        messages: Vec::new(),
                        before: 0,
                        mode,
                        prompt_text,
                    })
                    .await;
                } else {
                    self.fail("undo", Some(stored_id), message).await;
                }
            }
            branch_kind => self.fail_branch_job(branch_kind, code, message).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn fail_undo_job(
        &mut self,
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
        attempts: u8,
        code: i64,
        message: &str,
    ) {
        if code == 4009 && attempts < 3 {
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.send_undo(stored_id, runtime_id, mode, prompt_text, attempts + 1)
                .await;
            return;
        }
        self.fail("undo", Some(stored_id), message).await;
    }

    async fn fail_branch_job(&mut self, kind: JobKind, code: i64, message: &str) {
        match kind {
            JobKind::Branch {
                parent_stored_id,
                parent_title,
                fallback_title,
                fallback_seed,
            } => {
                if matches!(code, -32601 | 4007) && !fallback_seed.is_empty() {
                    self.begin_seeded_branch(
                        parent_stored_id,
                        parent_title,
                        fallback_title,
                        fallback_seed,
                    )
                    .await;
                } else {
                    self.fail("branch", Some(parent_stored_id), message).await;
                }
            }
            JobKind::BranchClose(pending) => {
                if code == -32000 {
                    self.fail("branch", Some(pending.parent_stored_id), message)
                        .await;
                } else {
                    self.reopen_branched(pending).await;
                }
            }
            JobKind::BranchResume(pending) => {
                self.fail("branch", Some(pending.parent_stored_id), message)
                    .await;
            }
            JobKind::BranchStored {
                parent_stored_id,
                parent_title,
                fallback_title,
                fallback_seed,
            } => {
                if code != -32000 && !fallback_seed.is_empty() {
                    self.begin_seeded_branch(
                        parent_stored_id,
                        parent_title,
                        fallback_title,
                        fallback_seed,
                    )
                    .await;
                } else {
                    self.fail("branch", Some(parent_stored_id), message).await;
                }
            }
            JobKind::BranchSeeded {
                parent_stored_id, ..
            } => self.fail("branch", Some(parent_stored_id), message).await,
            other => self.fail_curation_job(other, code, message).await,
        }
    }

    async fn fail_curation_job(&mut self, kind: JobKind, code: i64, message: &str) {
        match kind {
            JobKind::ArchiveClose { stored_id, retried } => {
                self.request_archive_patch(stored_id, retried).await;
            }
            JobKind::ArchiveResume { stored_id } => {
                if matches!(resume_disposition(code), ResumeDisposition::ReadOnly) {
                    self.refused.insert(stored_id.clone());
                }
                self.fail("curation", Some(stored_id), message).await;
            }
            JobKind::Compress { stored_id, .. } | JobKind::CompressSync { stored_id, .. } => {
                self.fail("compress", Some(stored_id), message).await;
            }
            _ => {}
        }
    }

    async fn fail_submit_job(&mut self, runtime_id: String, generation: u64, message: &str) {
        let applies = self
            .turns
            .get_mut(&runtime_id)
            .is_some_and(|turn| turn.fail_submit(generation));
        if applies {
            self.fail(
                "submit",
                Some(runtime_id),
                format!("{message} The prompt will not be sent again."),
            )
            .await;
        }
    }

    async fn fail_interrupt_job(&self, runtime_id: String, generation: u64, message: &str) {
        if self
            .turns
            .get(&runtime_id)
            .is_some_and(|turn| turn.fail_interrupt(generation))
        {
            self.fail("interrupt", Some(runtime_id), message).await;
        }
    }

    async fn fail_side_job(&self, runtime_id: String, client_id: u64, code: i64, message: &str) {
        let detail = if code == -32601 {
            "This Hermes gateway does not expose prompt.btw yet (requires Hermes v0.23+)."
                .to_string()
        } else {
            self.clean(message.to_string())
        };
        self.emit(Event::SideFailed {
            runtime_id,
            client_id,
            message: detail,
        })
        .await;
    }

    async fn fail_delete_job(&mut self, kind: JobKind, code: i64, message: &str) {
        match kind {
            JobKind::Close { stored_id, retried } => self.request_delete(stored_id, retried).await,
            JobKind::DeleteResume { stored_id } => {
                if matches!(resume_disposition(code), ResumeDisposition::ReadOnly) {
                    self.refused.insert(stored_id.clone());
                }
                self.fail("delete", Some(stored_id), message).await;
            }
            JobKind::Delete { stored_id, retried } => {
                if code == 4132 {
                    self.refused.insert(stored_id.clone());
                }
                if code == 4023
                    && !retried
                    && !self.refused.contains(&stored_id)
                    && self
                        .send_rpc(
                            "session.resume",
                            json!({
                                "session_id": stored_id,
                                "inline_images": false,
                                "omit_messages": true
                            }),
                            JobKind::DeleteResume {
                                stored_id: stored_id.clone(),
                            },
                            RPC_TIMEOUT,
                        )
                        .await
                        .is_ok()
                {
                    return;
                }
                self.fail("delete", Some(stored_id), message).await;
            }
            _ => {}
        }
    }

    async fn on_created(&mut self, title: String, result: &Value) {
        let runtime_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let stored_id = result
            .get("stored_session_id")
            .or_else(|| result.get("session_key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if runtime_id.is_empty() || stored_id.is_empty() {
            self.fail(
                "create",
                None,
                "The gateway did not return a conversation id.",
            )
            .await;
            return;
        }
        self.runtime_of
            .insert(stored_id.clone(), runtime_id.clone());
        self.message_counts.insert(stored_id.clone(), 0);
        let title = if title.trim().is_empty() {
            "New conversation".into()
        } else {
            title
        };
        let model = model_identity(result);
        let model_name = model
            .as_ref()
            .map_or_else(String::new, |entry| entry.model.clone());
        self.emit(Event::Created {
            session: SessionSummary {
                stored_id,
                title,
                source: "tui".into(),
                updated_at: crate::palette::now_epoch_secs().to_string(),
                read_only_reason: None,
                message_count: 0,
                parent_session_id: None,
                is_branch: false,
                pinned: false,
                preview: String::new(),
                model: model_name,
            },
            runtime_id: runtime_id.clone(),
            model,
        })
        .await;
        self.emit_live_info(&runtime_id, result).await;
        self.request_session_info_if_needed(&runtime_id, result)
            .await;
    }

    async fn request_session_info_if_needed(&mut self, runtime_id: &str, result: &Value) {
        if runtime_id.is_empty()
            || parse_live_info_telemetry(result).is_some_and(|t| !t.cwd.is_empty())
        {
            return;
        }
        let _ = self
            .send_rpc(
                "session.info",
                json!({"session_id": runtime_id}),
                JobKind::SessionInfo {
                    runtime_id: runtime_id.to_string(),
                },
                RPC_TIMEOUT,
            )
            .await;
    }

    async fn on_resumed(&mut self, stored_id: String, result: &Value) {
        let runtime_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(runtime_id) = &runtime_id {
            self.runtime_of
                .insert(stored_id.clone(), runtime_id.clone());
        }
        let Some(count) = result
            .get("message_count")
            .and_then(Value::as_u64)
            .map(|count| count as usize)
        else {
            self.fail(
                "resume",
                Some(stored_id),
                "The gateway did not return a history row count.",
            )
            .await;
            return;
        };
        self.message_counts.insert(stored_id.clone(), count);
        let before = count.saturating_sub(PAGE);
        // The saved page loads off the transport loop — a slow history
        // endpoint must not stall frames on the most common open path. The
        // reply travels with the fetch so the continuation replays exactly:
        // Resumed, live info, turn adoption, open requests.
        tokio::spawn(resumed_history_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            ResumedPage {
                stored_id,
                runtime_id,
                before,
                // The task overwrites this with the fetched rows.
                messages: Ok(Vec::new()),
                result: result.clone(),
            },
            count - before,
        ));
    }

    /// Applies a finished resume page on the loop: the same continuation the
    /// inline path ran after the fetch.
    async fn apply_resumed(&mut self, page: ResumedPage) {
        let ResumedPage {
            stored_id,
            runtime_id,
            before,
            messages,
            result,
        } = page;
        let messages = match messages {
            Ok(messages) => messages,
            Err(message) => {
                self.fail("resume", Some(stored_id), message).await;
                return;
            }
        };
        let running = is_resumed_running(&result);
        self.emit(Event::Resumed {
            stored_id,
            runtime_id: runtime_id.clone(),
            messages,
            before,
            read_only_reason: None,
            model: model_identity(&result),
        })
        .await;
        if let Some(rt) = &runtime_id {
            self.emit_live_info(rt, &result).await;
            self.request_session_info_if_needed(rt, &result).await;
        }
        if running && let Some(runtime_id) = runtime_id.clone() {
            self.next_turn_generation += 1;
            let generation = self.next_turn_generation;
            self.turns.insert(
                runtime_id.clone(),
                Turn::streaming(format!("turn-{runtime_id}-{generation}").into(), generation),
            );
            self.emit(Event::Submitted { runtime_id }).await;
        }
        if let Some(open) = result.get("open_requests").and_then(Value::as_array) {
            for request in open {
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": request.get("params").cloned().unwrap_or(Value::Null)});
                self.on_server_request(method, &frame).await;
            }
        }
    }

    async fn resume_refused(&mut self, stored_id: String, code: i64, message: &str) {
        match resume_disposition(code) {
            ResumeDisposition::Missing => {
                self.fail("resume", Some(stored_id), "Session not found.")
                    .await;
            }
            ResumeDisposition::Retryable => {
                self.fail(
                    "resume",
                    Some(stored_id),
                    format!("{message} Reconnect to try again."),
                )
                .await;
            }
            ResumeDisposition::ReadOnly => self.show_readonly(stored_id, message).await,
        }
    }

    fn note_listed(&mut self, session: &SessionSummary) {
        self.message_counts
            .insert(session.stored_id.clone(), session.message_count);
        self.set_pinned_cache(&session.stored_id, session.pinned);
        if session.source == "kanban" {
            self.refused.insert(session.stored_id.clone());
        }
    }

    /// A refused resume loads the saved transcript off the transport loop:
    /// count discovery (a session-list walk when the count is uncached) and
    /// the saved page must not stall frames. The outcome re-enters via the
    /// work channel, where the listing caches warm from the walked rows.
    async fn show_readonly(&mut self, stored_id: String, message: &str) {
        self.refused.insert(stored_id.clone());
        let reason = self.clean(message.to_string());
        let cached = self.message_counts.get(&stored_id).copied();
        tokio::spawn(readonly_history_task(
            self.http.clone(),
            self.origin.clone(),
            self.work_sink(),
            stored_id,
            reason,
            cached,
        ));
    }

    /// Applies a finished refused-resume load on the loop: the walked rows
    /// warm the listing caches exactly as the inline pagination did, then
    /// the same `Resumed` (or resume failure) the inline path emitted.
    async fn apply_readonly(&mut self, outcome: ReadonlyOutcome) {
        let ReadonlyOutcome {
            stored_id,
            reason,
            result,
        } = outcome;
        match result {
            Ok(readonly) => {
                let ReadonlyHistory {
                    before,
                    messages,
                    warmed,
                } = readonly;
                for session in &warmed {
                    self.note_listed(session);
                }
                self.emit(Event::Resumed {
                    stored_id,
                    runtime_id: None,
                    messages,
                    before,
                    read_only_reason: Some(if reason.is_empty() {
                        "This conversation cannot be continued. Showing the saved transcript."
                            .into()
                    } else {
                        reason
                    }),
                    model: None,
                })
                .await;
            }
            Err(error) => {
                self.fail(
                    "resume",
                    Some(stored_id),
                    format!("{reason} Saved history could not be loaded: {error}"),
                )
                .await;
            }
        }
    }


    async fn maintain(&mut self) {
        // Parked streaming tails get a retry on every tick; a slow UI must not
        // stall the transport loop (see emit_delta).
        self.flush_pending_streams();
        let now = Instant::now();
        self.fail_expired_jobs(now).await;
        if self.write.is_some()
            && !self.ready
            && self
                .opened_at
                .is_some_and(|opened| opened.elapsed() > READY_TIMEOUT)
        {
            self.drop_socket("The gateway did not become ready.").await;
            return;
        }
        self.probe_keepalive(now).await;
        if self.write.is_some() && self.last_inbound.elapsed() > SILENCE_BOUND {
            self.drop_socket("The gateway stopped responding.").await;
            return;
        }
        self.heartbeat_ping().await;
    }

    /// Fails every job whose response deadline passed on this tick.
    async fn fail_expired_jobs(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(job) = self.jobs.remove(&id) {
                let kind = job.kind;
                self.on_rpc_error(
                    Job {
                        kind,
                        deadline: now,
                    },
                    &json!({"code": -32000, "message": "The gateway did not answer in time."}),
                )
                .await;
            }
        }
    }

    /// In heartbeat mode the gateway expects a periodic JSON-RPC ping once
    /// traffic has been quiet for 20s.
    async fn heartbeat_ping(&mut self) {
        if self.write.is_some()
            && self.heartbeat
            && self.last_inbound.elapsed() > HEARTBEAT_SILENCE
        {
            let _ = self
                .send_rpc("gateway.ping", json!({}), JobKind::Ping, RPC_TIMEOUT)
                .await;
        }
    }

    /// Retries every parked streaming tail (text deltas and reasoning) once
    /// per maintenance tick.
    fn flush_pending_streams(&mut self) {
        for runtime_id in self.pending_deltas.keys().cloned().collect::<Vec<_>>() {
            self.try_flush_delta(&runtime_id);
        }
        for runtime_id in self
            .pending_reasoning
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            self.try_flush_reasoning(&runtime_id);
        }
    }

    /// RFC 6455 keepalive: after `PROBE_SILENCE` without any inbound frame, send a
    /// protocol-level Ping. A live gateway answers with Pong, which refreshes
    /// `last_inbound`, so a healthy idle connection is no longer dropped by the
    /// inbound-silence bound. The socket is dropped only when the probe itself
    /// cannot be written.
    async fn probe_keepalive(&mut self, now: Instant) {
        if self.write.is_none() || !probe_due(self.last_inbound, now) {
            return;
        }
        let Some(write) = self.write.as_mut() else {
            return;
        };
        let sent =
            tokio::time::timeout(Duration::from_secs(5), write.send(Message::Ping(Vec::new().into())))
                .await;
        if !matches!(sent, Ok(Ok(()))) {
            self.drop_socket("The connection failed while probing.").await;
        }
    }

    async fn fail_jobs(&mut self, message: &str) {
        let jobs: Vec<Job> = self.jobs.drain().map(|(_, job)| job).collect();
        for job in jobs {
            self.on_rpc_error(job, &json!({"code": -32000, "message": message}))
                .await;
        }
    }

    async fn drop_socket(&mut self, message: &str) {
        self.close_socket().await;
        self.emit(Event::Connection(ConnectionState::Disconnected(
            message.into(),
        )))
        .await;
        self.fail_jobs(message).await;
    }

    async fn close_socket(&mut self) {
        if let Some(mut write) = self.write.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), write.send(Message::Close(None)))
                .await;
        }
        self.read = None;
        self.ready = false;
        self.opened_at = None;
        self.turns.clear();
        self.asks.clear();
        self.activity.clear();
        self.pending_deltas.clear();
        self.pending_reasoning.clear();
        self.runtime_of.clear();
        // In-flight patches from this connection are epoch-staled below;
        // their stored_ids must not stay marked as patching forever.
        self.patching.clear();
        // Stales every in-flight off-loop fetch: results stamped with an
        // earlier epoch are discarded instead of applied to the next
        // connection.
        self.epoch += 1;
    }

    /// Unwraps a work result stamped on a still-live connection. `None`
    /// means the fetch belonged to a connection that has since torn down;
    /// `run()` drops it rather than applying stale state.
    fn live_work(&self, stamped: StampedWork) -> Option<LoopWork> {
        (stamped.epoch == self.epoch).then_some(stamped.work)
    }

    /// The work channel stamped with the current connection epoch; task
    /// launchers hand this to spawned fetches.
    fn work_sink(&self) -> WorkSink {
        WorkSink {
            sender: self.work.clone(),
            epoch: self.epoch,
        }
    }
}


/// True when the socket has been silent long enough to warrant a keepalive
/// probe. The maintenance loop runs every 5s, so a live peer answers long
/// before `maintain` treats the link as dead.
fn probe_due(last_inbound: Instant, now: Instant) -> bool {
    now.duration_since(last_inbound) > PROBE_SILENCE
}

/// Keeps a parked delta tail under `cap` bytes, dropping the front. Trims on a
/// UTF-8 character boundary so the UI never receives half a character.
fn trim_to_cap(text: &mut String, cap: usize) {
    if text.len() <= cap {
        return;
    }
    let mut cut = text.len() - cap;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    let rest = text[cut..].to_string();
    *text = rest;
}

/// The WebSocket dial URL for a one-connection ticket: same origin with a
/// ws/wss scheme, the `/api/ws` path, and the ticket as the only query pair.
/// Returns `None` when the scheme flip is impossible for this origin.
fn ws_dial_url(origin: &url::Url, ticket: &str) -> Option<url::Url> {
    let mut url = origin.clone();
    let scheme = if origin.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).ok()?;
    url.set_path("/api/ws");
    url.query_pairs_mut()
        .clear()
        .append_pair("ticket", ticket);
    Some(url)
}

fn http_client(origin: &url::Url) -> Result<reqwest::Client, String> {
    let expected = origin.clone();
    reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let next = attempt.url();
            let same = next.scheme() == expected.scheme()
                && next.host_str() == expected.host_str()
                && next.port_or_known_default() == expected.port_or_known_default();
            let clean = next.username().is_empty() && next.password().is_none();
            if same && clean && attempt.previous().len() < 3 {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "cross-origin redirect refused",
                ))
            }
        }))
        .timeout(RPC_TIMEOUT)
        .build()
        .map_err(|_| "Could not prepare the HTTP client.".to_string())
}

fn endpoint(origin: &url::Url, path: &str) -> url::Url {
    let mut url = origin.clone();
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url
}

async fn fetch_sessions(
    http: &reqwest::Client,
    origin: &url::Url,
    offset: usize,
) -> Result<Event, String> {
    let mut url = endpoint(origin, "/api/sessions");
    url.query_pairs_mut()
        .append_pair("limit", &PAGE.to_string())
        .append_pair("offset", &offset.to_string())
        .append_pair("order", "recent")
        .append_pair("archived", "exclude");
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not list conversations.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not list conversations (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "The conversation list was not usable.".to_string())?;
    parse_sessions(&body, offset)
}

fn parse_sessions(body: &Value, offset: usize) -> Result<Event, String> {
    let rows = body
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| "The conversation list was not usable.".to_string())?;
    let total = body
        .get("total")
        .and_then(Value::as_u64)
        .ok_or_else(|| "The conversation list was not usable.".to_string())?;
    let mut sessions = Vec::with_capacity(rows.len());
    for row in rows {
        sessions.push(session_summary(row)?);
    }
    if rows.is_empty() && (offset as u64) < total {
        return Err("The conversation list was not usable.".into());
    }
    let has_more = (offset as u64).saturating_add(sessions.len() as u64) < total;
    Ok(Event::Sessions {
        sessions,
        offset,
        has_more,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchOutcome {
    Applied,
    NotFound,
    Conflict,
}

fn is_queueable_while_connecting(command: &Command) -> bool {
    matches!(
        command,
        Command::List { .. }
            | Command::Create { .. }
            | Command::Resume { .. }
            | Command::SearchSessions { .. }
            | Command::FetchStudio
    )
}

fn unready_session_failure(command: &Command) -> Option<(&'static str, String)> {
    match command {
        Command::Undo { stored_id, .. } | Command::Branch { stored_id, .. } => {
            Some(("timeline", stored_id.clone()))
        }
        Command::Compress { stored_id, .. } => Some(("compress", stored_id.clone())),
        Command::MutateSession { stored_id, .. } => Some(("curation", stored_id.clone())),
        _ => None,
    }
}

fn deferred_patch_body(title: Option<String>, pinned: Option<bool>) -> Option<Value> {
    let mut map = serde_json::Map::new();
    if let Some(title) = title {
        map.insert("title".into(), Value::String(title));
    }
    if let Some(pinned) = pinned {
        map.insert("pinned".into(), Value::Bool(pinned));
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

async fn fetch_search_hits(
    http: &reqwest::Client,
    origin: &url::Url,
    query: &str,
    secrets: &[String],
) -> Result<Vec<crate::model::SearchHit>, String> {
    let mut url = endpoint(origin, "/api/sessions/search");
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", &SEARCH_LIMIT.to_string());
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not search conversations.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not search conversations (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Search results were not usable.".to_string())?;
    recall::parse_search_results(&body, secrets)
}

fn next_mutation_request_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("hn-mut-{nanos}-{seq}")
}

fn apply_mutation_snapshot(body: &Value, snapshot: &Value) -> Value {
    let Some(mut map) = body.as_object().cloned() else {
        return body.clone();
    };
    let Some(revision) = snapshot.get("runtime_revision").cloned() else {
        return body.clone();
    };
    let generation = snapshot
        .get("runtime_generation")
        .cloned()
        .unwrap_or(Value::Null);
    map.insert(
        "request_id".into(),
        Value::String(next_mutation_request_id()),
    );
    map.insert("expected_revision".into(), revision);
    map.insert("expected_generation".into(), generation);
    Value::Object(map)
}

async fn build_fenced_patch_body(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    body: &Value,
) -> Value {
    let snap_path = format!("/api/sessions/{stored_id}/mutation-snapshot");
    let Ok(resp) = http.get(endpoint(origin, &snap_path)).send().await else {
        return body.clone();
    };
    if !resp.status().is_success() {
        return body.clone();
    };
    let Ok(snapshot) = resp.json::<Value>().await else {
        return body.clone();
    };
    apply_mutation_snapshot(body, &snapshot)
}

async fn send_fenced_patch(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    body: &Value,
) -> Result<PatchOutcome, String> {
    let payload = build_fenced_patch_body(http, origin, stored_id, body).await;
    let path = format!("/api/sessions/{stored_id}");
    let response = http
        .patch(endpoint(origin, &path))
        .json(&payload)
        .send()
        .await
        .map_err(|_| "Could not update conversation.".to_string())?;
    let code = response.status().as_u16();
    if response.status().is_success() {
        return Ok(PatchOutcome::Applied);
    }
    if code == 404 {
        return Ok(PatchOutcome::NotFound);
    }
    if code == 409 {
        return Ok(PatchOutcome::Conflict);
    }
    Err(format!("Could not update conversation (HTTP {code})."))
}

async fn patch_session_http(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    body: &Value,
) -> Result<PatchOutcome, String> {
    let first = send_fenced_patch(http, origin, stored_id, body).await?;
    if first != PatchOutcome::Conflict {
        return Ok(first);
    }
    send_fenced_patch(http, origin, stored_id, body).await
}

/// The earlier-visible pages for a branch request, fetched off the transport
/// loop: up to ceil(before_loaded / PAGE) history round trips that must not
/// stall frames while a conversation is branched.
async fn branch_seeds_task(
    http: reqwest::Client,
    origin: url::Url,
    before_loaded: usize,
    work: WorkSink,
    request: BranchSeeds,
) {
    let seeds = fetch_earlier_pages(&http, &origin, &request.stored_id, before_loaded).await;
    work.deliver(LoopWork::Branch(Box::new(BranchSeedsOutcome {
        request,
        seeds,
    })));
}

async fn fetch_earlier_pages(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    before_loaded: usize,
) -> Result<Vec<SeedMessage>, String> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < before_loaded {
        let limit = (before_loaded - offset).min(PAGE);
        let page = fetch_history(http, origin, stored_id, offset, limit).await?;
        out.extend(seed_messages_through(&page, None));
        offset += limit;
    }
    Ok(out)
}

/// The saved-transcript load for a refused resume, off the transport loop:
/// count discovery (skipped when the count was cached) plus the history
/// page. The loop warms its listing caches from the walked rows.
async fn readonly_history_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    stored_id: String,
    reason: String,
    cached: Option<usize>,
) {
    let result = readonly_history(&http, &origin, &stored_id, cached).await;
    work.deliver(LoopWork::Readonly(Box::new(ReadonlyOutcome {
        stored_id,
        reason,
        result,
    })));
}

async fn readonly_history(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    cached: Option<usize>,
) -> Result<ReadonlyHistory, String> {
    let mut warmed = Vec::new();
    let count = match cached {
        Some(count) => count,
        None => {
            // Same walk the inline pagination did, minus the cache writes:
            // the rows travel back and the loop warms from them.
            let mut offset = 0;
            loop {
                let Event::Sessions {
                    sessions, has_more, ..
                } = fetch_sessions(http, origin, offset).await?
                else {
                    return Err("The conversation list was not usable.".into());
                };
                warmed.extend(sessions.iter().cloned());
                if let Some(count) = sessions
                    .iter()
                    .find(|session| session.stored_id == stored_id)
                    .map(|session| session.message_count)
                {
                    break count;
                }
                if sessions.is_empty() || !has_more {
                    return Err("Session metadata was not found.".into());
                }
                let next = offset.saturating_add(sessions.len());
                if next == offset {
                    return Err("Session metadata was not found.".into());
                }
                offset = next;
            }
        }
    };
    let before = count.saturating_sub(PAGE);
    let messages = fetch_history(http, origin, stored_id, before, count - before).await?;
    Ok(ReadonlyHistory {
        before,
        messages,
        warmed,
    })
}

/// The saved page for a successful resume, off the transport loop: the
/// continuation replays on the loop with the original reply in hand.
async fn resumed_history_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    mut page: ResumedPage,
    limit: usize,
) {
    let messages = fetch_history(&http, &origin, &page.stored_id, page.before, limit).await;
    page.messages = messages;
    work.deliver(LoopWork::Resumed(Box::new(page)));
}

/// A post-sync history page (undo or compress), off the transport loop; the
/// completion event applies on the loop when the rows return.
async fn synced_page_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    mut page: SyncedPage,
    limit: usize,
) {
    page.messages = fetch_history(&http, &origin, &page.stored_id, page.before, limit).await;
    work.deliver(LoopWork::Page(Box::new(page)));
}

/// The branched conversation's first page, off the transport loop; on error
/// or an empty page the fallback seed rides instead.
async fn branched_page_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    mut page: BranchedPage,
    limit: usize,
) {
    page.messages = match fetch_history(&http, &origin, &page.stored_id, page.before, limit).await {
        Ok(rows) if !rows.is_empty() => rows,
        _ => std::mem::take(&mut page.fallback_messages),
    };
    work.deliver(LoopWork::Branched(Box::new(page)));
}

/// The archive patch as its own task: the fenced patch (and its one
/// conflict retry) runs off the transport loop, and the outcome re-enters
/// via the work channel where archive-completion state applies it.
async fn archive_patch_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    stored_id: String,
    retried: bool,
    body: Value,
) {
    let result = patch_session_http(&http, &origin, &stored_id, &body).await;
    work.deliver(LoopWork::Archived {
        stored_id,
        retried,
        result,
    });
}

/// The session patch as its own task: the fenced patch (and its one
/// conflict retry) runs off the transport loop, and the outcome re-enters
/// via the work channel where curation state applies it.
async fn patch_session_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    stored_id: String,
    mutation: SessionMutation,
    body: Value,
) {
    let result = patch_session_http(&http, &origin, &stored_id, &body).await;
    work.deliver(LoopWork::Mutated {
        stored_id,
        mutation,
        result,
    });
}

/// A deferred title/pin patch as its own task: unlike the old
/// fire-and-forget flush, the outcome re-enters the loop, where a refused
/// or failed save re-defers the user's intent and surfaces instead of
/// silently reverting on the next list refresh.
async fn deferred_patch_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    stored_id: String,
    title: Option<String>,
    pinned: Option<bool>,
    body: Value,
) {
    let result = patch_session_http(&http, &origin, &stored_id, &body).await;
    work.deliver(LoopWork::Deferred {
        stored_id,
        title,
        pinned,
        result,
    });
}

async fn fetch_workspaces_http(
    http: &reqwest::Client,
    origin: &url::Url,
    secrets: &[String],
) -> Result<WorkspaceCatalog, String> {
    let response = http
        .get(endpoint(origin, "/api/chat/workspaces"))
        .send()
        .await
        .map_err(|_| "Could not load remote workspaces.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not load remote workspaces (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Workspace catalog was not usable.".to_string())?;
    studio::parse_workspaces(&body, secrets)
}

async fn fetch_skills_http(
    http: &reqwest::Client,
    origin: &url::Url,
    secrets: &[String],
) -> Result<Vec<SkillSummary>, String> {
    let response = http
        .get(endpoint(origin, "/api/skills"))
        .send()
        .await
        .map_err(|_| "Could not load remote skills.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not load remote skills (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Skills catalog was not usable.".to_string())?;
    studio::parse_skills(&body, secrets)
}

async fn upload_chat_image_http(
    http: &reqwest::Client,
    origin: &url::Url,
    image: &StagedImage,
) -> Result<String, String> {
    let response = http
        .post(endpoint(origin, "/api/chat/image-upload"))
        .json(&json!({
            "data_url": image.data_url(),
            "filename": image.filename,
        }))
        .send()
        .await
        .map_err(|_| format!("Could not upload {}.", image.filename))?;
    if !response.status().is_success() {
        return Err(format!(
            "Image upload failed for {} (HTTP {}).",
            image.filename,
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Image upload reply was not usable.".to_string())?;
    body.get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Gateway did not return an uploaded image path.".into())
}

/// The submit path's image uploads as their own task: concurrent (N images
/// cost one wall-clock window instead of N serial round trips) and off the
/// transport loop. The uploaded paths — or the first failure — re-enter the
/// loop via the work channel, where the generation guard discards stale
/// results.
async fn upload_images_task(
    http: reqwest::Client,
    origin: url::Url,
    work: WorkSink,
    runtime_id: String,
    generation: u64,
    images: Vec<StagedImage>,
    prompt_text: String,
) {
    let result = futures_util::future::try_join_all(
        images
            .iter()
            .map(|image| upload_chat_image_http(&http, &origin, image)),
    )
    .await
    .map(VecDeque::from);
    work.deliver(LoopWork::Uploaded {
        runtime_id,
        generation,
        result,
        prompt_text,
    });
}

/// The catalog fetch body that runs as its own task: two independent HTTP
/// calls (concurrent halves the wall time), whose results land on the event
/// channel with the same redaction the inline version used.
async fn fetch_studio_catalogs_task(
    http: reqwest::Client,
    origin: url::Url,
    creds: Vec<String>,
    secrets: Vec<String>,
    events: tokio::sync::mpsc::Sender<Event>,
) {
    let (workspaces, skills) = tokio::join!(
        fetch_workspaces_http(&http, &origin, &creds),
        fetch_skills_http(&http, &origin, &creds),
    );
    let catalog = match workspaces {
        Ok(catalog) => catalog,
        Err(err) => WorkspaceCatalog {
            error: Some(sanitize(&err, &secrets)),
            ..Default::default()
        },
    };
    let _ = events.send(Event::Workspaces { catalog }).await;
    if let Ok(skills) = skills {
        let _ = events.send(Event::Skills { skills }).await;
    }
}

/// The history read as its own task: same events, same redaction, emitted
/// directly on the bounded channel.
async fn fetch_history_task(
    http: reqwest::Client,
    origin: url::Url,
    secrets: Vec<String>,
    events: tokio::sync::mpsc::Sender<Event>,
    stored_id: String,
    before: usize,
) {
    let next_before = before.saturating_sub(PAGE);
    let result = fetch_history(
        &http,
        &origin,
        &stored_id,
        next_before,
        before - next_before,
    )
    .await;
    match result {
        Ok(messages) => {
            let _ = events
                .send(Event::History {
                    stored_id,
                    messages,
                    before,
                    next_before,
                })
                .await;
        }
        Err(message) => {
            let _ = events
                .send(Event::Error {
                    operation: "history".into(),
                    session_id: Some(stored_id),
                    message: sanitize(&message, &secrets),
                })
                .await;
        }
    }
}

/// The search read as its own task: pinned marking against the cloned pin
/// set, and on failure an empty result set followed by the same error event
/// the inline path emitted.
async fn fetch_search_task(
    http: reqwest::Client,
    origin: url::Url,
    secrets: Vec<String>,
    pinned: std::collections::HashSet<String>,
    events: tokio::sync::mpsc::Sender<Event>,
    query: String,
    request_seq: u64,
) {
    let trimmed = query.trim().to_string();
    match fetch_search_hits(&http, &origin, &trimmed, &secrets).await {
        Ok(mut hits) => {
            for hit in &mut hits {
                if pinned.contains(&hit.stored_id) {
                    hit.pinned = true;
                }
            }
            let _ = events
                .send(Event::SearchResults {
                    query,
                    request_seq,
                    hits,
                })
                .await;
        }
        Err(message) => {
            let _ = events
                .send(Event::SearchResults {
                    query,
                    request_seq,
                    hits: Vec::new(),
                })
                .await;
            let _ = events
                .send(Event::Error {
                    operation: "search".into(),
                    session_id: None,
                    message: sanitize(&message, &secrets),
                })
                .await;
        }
    }
}

fn session_summary(row: &Value) -> Result<SessionSummary, String> {
    let stored_id = optional_trimmed_str(row, "id")
        .ok_or_else(|| "The conversation list was not usable.".to_string())?;
    let count = row
        .get("message_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "The conversation list was not usable.".to_string())?;
    let message_count =
        usize::try_from(count).map_err(|_| "The conversation list was not usable.".to_string())?;
    let title = session_summary_title(row);
    let raw_preview = row.get("preview").and_then(Value::as_str).unwrap_or("");
    Ok(SessionSummary {
        stored_id,
        preview: recall::clean_session_preview(raw_preview, &title),
        title,
        source: optional_trimmed_str(row, "source").unwrap_or_default(),
        updated_at: extract_epoch_string(row),
        read_only_reason: None,
        message_count,
        parent_session_id: optional_trimmed_str(row, "parent_session_id"),
        is_branch: false,
        pinned: row.get("pinned").and_then(Value::as_bool).unwrap_or(false),
        model: optional_trimmed_str(row, "model").unwrap_or_default(),
    })
}

fn session_summary_title(row: &Value) -> String {
    match optional_trimmed_str(row, "title") {
        Some(title) => title,
        None => "Untitled".into(),
    }
}

fn optional_trimmed_str(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn value_as_epoch(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                .filter(|f| f.is_finite() && *f > 0.0)
                .map(|f| f as i64)
        })
        .filter(|&n| n > 0)
}

fn extract_epoch_string(row: &Value) -> String {
    [
        "last_active",
        "last_activity_at",
        "updated_at",
        "ended_at",
        "started_at",
    ]
    .into_iter()
    .find_map(|key| row.get(key).and_then(value_as_epoch))
    .map(|n| n.to_string())
    .unwrap_or_default()
}

async fn fetch_history(
    http: &reqwest::Client,
    origin: &url::Url,
    stored_id: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<ChatMessage>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut url = endpoint(
        origin,
        &format!("/api/sessions/{}/messages", path_segment(stored_id)),
    );
    url.query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("offset", &offset.to_string())
        .append_pair("order", "oldest")
        .append_pair("inline_images", "false");
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not load history.".to_string())?;
    if response.status().as_u16() == 404 {
        return Err("Saved history was not found.".into());
    }
    if !response.status().is_success() {
        return Err(format!(
            "Could not load history (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "History was not usable.".to_string())?;
    history_messages(&body, limit)
}

fn history_messages(body: &Value, limit: usize) -> Result<Vec<ChatMessage>, String> {
    let rows = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "The gateway did not return saved message rows.".to_string())?;
    if rows.len() != limit {
        return Err("The history page was incomplete.".into());
    }
    if let Some(order) = body
        .get("pagination")
        .and_then(|page| page.get("order"))
        .and_then(Value::as_str)
        && order != "oldest"
    {
        return Err("The history page was not chronological.".into());
    }
    if let Some(returned) = body
        .get("pagination")
        .and_then(|page| page.get("returned"))
        .and_then(Value::as_u64)
        && usize::try_from(returned).ok() != Some(rows.len())
    {
        return Err("The history page was incomplete.".into());
    }
    Ok(messages_from(rows))
}

fn messages_from(rows: &[Value]) -> Vec<ChatMessage> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let role = role_of(row.get("role").and_then(Value::as_str).unwrap_or(""));
            let text = if role == Role::Tool {
                match row
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    Some(name) => format!("Used {name}"),
                    None => "Tool called".to_string(),
                }
            } else {
                message_text(row)
            };
            ChatMessage {
                id: message_id(row, index),
                role,
                text,
            }
        })
        .collect()
}

fn message_id(row: &Value, index: usize) -> String {
    if let Some(id) = row
        .get("message_uid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return id.to_string();
    }
    if let Some(id) = row.get("row_id").and_then(Value::as_i64) {
        return format!("row-{id}");
    }
    if let Some(id) = row.get("id").and_then(Value::as_i64) {
        return format!("row-{id}");
    }
    if let Some(id) = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return id.to_string();
    }
    format!("msg-{index}")
}

fn message_text(row: &Value) -> String {
    for key in ["text", "content", "display_content"] {
        if let Some(text) = row.get(key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

fn role_of(role: &str) -> Role {
    match role.to_ascii_lowercase().as_str() {
        "user" | "human" => Role::User,
        "assistant" | "model" => Role::Assistant,
        "tool" | "function" => Role::Tool,
        _ => Role::System,
    }
}

/// The model a create or resume reply reports. `info` carries the live or restored pair;
/// a lazy reply may only carry the top-level default, which is not proof of this chat.
fn model_identity(result: &Value) -> Option<ModelIdentity> {
    let info = result.get("info").unwrap_or(result);
    ModelIdentity::parse(
        info.get("provider").and_then(Value::as_str),
        info.get("model").and_then(Value::as_str),
    )
    .or_else(|| {
        ModelIdentity::parse(
            result.get("provider").and_then(Value::as_str),
            result.get("model").and_then(Value::as_str),
        )
    })
}

fn turn_status(status: &str) -> TurnStatus {
    match status.to_ascii_lowercase().as_str() {
        "complete" | "completed" => TurnStatus::Complete,
        "interrupted" | "cancelled" | "canceled" => TurnStatus::Interrupted,
        "failed" | "error" => TurnStatus::Failed,
        "streaming" | "running" => TurnStatus::Streaming,
        "awaiting_input" | "approval" | "clarify" => TurnStatus::AwaitingInput,
        "" => TurnStatus::Unknown,
        _ => TurnStatus::Unknown,
    }
}

fn approval_prompt(params: &Value) -> String {
    let description = params
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let command = params.get("command").and_then(Value::as_str).unwrap_or("");
    match (description.is_empty(), command.is_empty()) {
        (false, false) => format!("{description}\n{command}"),
        (false, true) => description.to_string(),
        (true, false) => command.to_string(),
        (true, true) => "Hermes is asking to run a tool.".into(),
    }
}

fn approval_choices(params: &Value) -> Vec<String> {
    let offered = string_list(params.get("choices"))
        .unwrap_or_default()
        .into_iter()
        .filter(|choice| matches!(choice.as_str(), "once" | "session" | "always" | "deny"))
        .collect::<Vec<_>>();
    if offered.is_empty() {
        vec!["once".into(), "deny".into()]
    } else {
        offered
    }
}

fn request_key(id: &Value) -> Option<String> {
    if let Some(text) = id.as_str().filter(|text| !text.is_empty()) {
        return Some(format!("s:{text}"));
    }
    if id.is_number() {
        return Some(format!("n:{id}"));
    }
    None
}

fn same_request_id(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (request_text(left), request_text(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn request_text(id: &Value) -> Option<String> {
    id.as_str()
        .map(str::to_string)
        .or_else(|| id.as_i64().map(|number| number.to_string()))
        .or_else(|| id.as_u64().map(|number| number.to_string()))
}

fn clarification_questions(params: &Value) -> Result<Vec<ClarificationQuestion>, String> {
    let rows = params
        .get("questions")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| "The clarification has no questions.".to_string())?;
    let mut questions = Vec::with_capacity(rows.len());
    for row in rows {
        let qid = row
            .get("qid")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "A clarification question has no qid.".to_string())?;
        let prompt = row
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| "A clarification question has no text.".to_string())?;
        if questions
            .iter()
            .any(|question: &ClarificationQuestion| question.qid == qid)
        {
            return Err("The server repeated a clarification qid.".into());
        }
        questions.push(ClarificationQuestion {
            qid: qid.into(),
            prompt: prompt.into(),
            choices: string_list(row.get("choices")).unwrap_or_default(),
            multi_select: row
                .get("multi_select")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(questions)
}

fn locked_answers(params: &Value) -> Result<HashMap<String, Option<String>>, String> {
    let Some(raw) = params.get("answers") else {
        return Ok(HashMap::new());
    };
    let Some(object) = raw.as_object() else {
        return Err("Replayed clarification answers were not usable.".into());
    };
    let mut locked = HashMap::new();
    for (qid, value) in object {
        let parsed = match value {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            _ => return Err("Replayed clarification answers must be text or null.".into()),
        };
        locked.insert(qid.clone(), parsed);
    }
    Ok(locked)
}

fn interaction_result(
    method: &str,
    params: &Value,
    answer: &InteractionAnswer,
) -> Result<Value, String> {
    match (method, answer) {
        ("approval", InteractionAnswer::Approval(choice)) => {
            if !approval_choices(params)
                .iter()
                .any(|offered| offered == choice)
            {
                return Err("The response does not match the server's offered choices.".into());
            }
            Ok(json!({"choice": choice}))
        }
        ("approval", InteractionAnswer::Cancel) => {
            if !approval_choices(params)
                .iter()
                .any(|choice| choice == "deny")
            {
                return Err("This approval has no deny choice.".into());
            }
            Ok(json!({"choice": "deny"}))
        }
        ("clarify", InteractionAnswer::Cancel) => Ok(json!({})),
        ("clarify", InteractionAnswer::Clarification(answers)) => clarify_answers(params, answers),
        _ => Err("The response does not match this question.".into()),
    }
}

fn clarify_answers(
    params: &Value,
    answers: &HashMap<String, Option<String>>,
) -> Result<Value, String> {
    let questions = clarification_questions(params)?;
    let locked = locked_answers(params)?;
    if answers
        .keys()
        .any(|qid| !questions.iter().any(|question| &question.qid == qid))
    {
        return Err("The answer names an unknown clarification question.".into());
    }
    let mut out = serde_json::Map::new();
    for question in &questions {
        let provided = answers.get(&question.qid);
        let value = match (locked.get(&question.qid), provided) {
            (Some(locked_value), Some(provided)) if provided != locked_value => {
                return Err("A replayed answer is locked and cannot be edited.".into());
            }
            (Some(locked_value), _) => locked_value.clone(),
            (None, Some(provided)) => provided.clone(),
            (None, None) => {
                return Err(format!(
                    "Question {} needs its own answer or an explicit skip.",
                    question.qid
                ));
            }
        };
        if !locked.contains_key(&question.qid) {
            validate_clarify_value(question, value.as_deref())?;
        }
        out.insert(
            question.qid.clone(),
            match value {
                Some(text) => Value::String(text),
                None => Value::Null,
            },
        );
    }
    Ok(json!({"answers": Value::Object(out)}))
}

fn validate_clarify_value(
    question: &ClarificationQuestion,
    value: Option<&str>,
) -> Result<(), String> {
    let Some(text) = value else {
        return Ok(());
    };
    if question.choices.is_empty() || question.choices.iter().any(|choice| choice == text) {
        return Ok(());
    }
    if question.multi_select && multi_select_matches(&question.choices, text) {
        return Ok(());
    }
    Err("The answer does not match the offered choices.".into())
}

fn multi_select_matches(choices: &[String], text: &str) -> bool {
    let Ok(items) = serde_json::from_str::<Vec<String>>(text) else {
        return false;
    };
    !items.is_empty()
        && items
            .iter()
            .all(|part| choices.iter().any(|choice| choice == part))
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id.clone(), "result": result})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.clone(),
        "error": {"code": code, "message": message}
    })
}

enum SendFault {
    NotSent,
    Ambiguous,
}

fn send_fault_message(operation: &str, fault: SendFault) -> String {
    match (operation, fault) {
        ("submit", SendFault::NotSent) => "Not connected. The prompt was not sent.".into(),
        ("submit", SendFault::Ambiguous) => "Sending failed. The server may have received this prompt; it will not be sent again. Reconnect to reload history.".into(),
        ("create", SendFault::NotSent) => "Not connected.".into(),
        ("create", SendFault::Ambiguous) => "The create request may have reached the server. It will not be sent again.".into(),
        ("resume", SendFault::NotSent) => "Not connected. Reconnect to try again.".into(),
        ("resume", SendFault::Ambiguous) => "The connection failed while requesting history. Reconnect to try again.".into(),
        ("interrupt", SendFault::NotSent) => "Not connected.".into(),
        ("interrupt", SendFault::Ambiguous) => "The interrupt may not have reached the server.".into(),
        ("delete", SendFault::NotSent) => "Not connected.".into(),
        ("delete", SendFault::Ambiguous) => "The delete may not have reached the server. It will not be retried automatically.".into(),
        ("respond", SendFault::NotSent) => "Not connected. The answer was not sent.".into(),
        ("respond", SendFault::Ambiguous) => "The answer may not have reached the server. It will not be retried.".into(),
        (_, SendFault::NotSent) => "Not connected.".into(),
        (_, SendFault::Ambiguous) => "The connection failed while sending. It will not be retried.".into(),
    }
}

enum ResumeDisposition {
    Retryable,
    Missing,
    ReadOnly,
}

fn resume_disposition(code: i64) -> ResumeDisposition {
    if code == 4007 {
        ResumeDisposition::Missing
    } else if code == -32601 || code >= 4000 {
        ResumeDisposition::ReadOnly
    } else {
        ResumeDisposition::Retryable
    }
}

enum UnhandledGatewayEvent {
    Ignore,
    Malformed,
}

/// Event notifications this client does not render. A non-empty type is a
/// valid broadcast. Only a missing type is a protocol fault.
fn classify_unhandled_event(event_type: &str) -> UnhandledGatewayEvent {
    if event_type.is_empty() {
        UnhandledGatewayEvent::Malformed
    } else {
        UnhandledGatewayEvent::Ignore
    }
}

fn is_resumed_running(result: &Value) -> bool {
    result
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || matches!(
            result.get("status").and_then(Value::as_str),
            Some("streaming" | "running" | "resuming")
        )
        || result
            .get("inflight")
            .is_some_and(|value| value.is_object())
}

fn tool_name_from(payload: &Value) -> &str {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .or_else(|| payload.get("tool_name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("tool")
}

fn payload_str(payload: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| {
            payload
                .get(*k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_default()
}

fn parse_tool_event(
    event_type: &str,
    payload: &Value,
    secrets: &[String],
) -> Option<ToolEventUpdate> {
    let tool_id = payload_str(payload, &["tool_id", "call_id", "id"]);
    let name = tool_name_from(payload).to_string();
    match event_type {
        "tool.generating" | "tool.start" => Some(ToolEventUpdate::Started {
            tool_id,
            name,
            detail: compact_tool_detail(
                &payload_str(payload, &["preview", "context", "args_text"]),
                secrets,
            ),
        }),
        "tool.progress" => Some(ToolEventUpdate::Progress {
            tool_id,
            name,
            percent: extract_progress_pct(payload),
            message: compact_tool_detail(
                &payload_str(payload, &["message", "text", "status"]),
                secrets,
            ),
        }),
        "tool.output_risk" => {
            let raw = payload_str(payload, &["risk", "reason", "level"]);
            Some(ToolEventUpdate::Risk {
                tool_id,
                name,
                risk: if raw.is_empty() {
                    "flagged output".into()
                } else {
                    compact_tool_detail(&raw, secrets)
                },
            })
        }
        "tool.complete" => Some(ToolEventUpdate::Completed {
            tool_id,
            name,
            duration_s: payload
                .get("duration_s")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0),
            summary: compact_tool_detail(&payload_str(payload, &["summary", "preview"]), secrets),
        }),
        _ => None,
    }
}

fn extract_progress_pct(payload: &Value) -> Option<u8> {
    payload
        .get("percent")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .map(|f| f.clamp(0.0, 100.0) as u8)
}

fn compact_tool_detail(raw: &str, secrets: &[String]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let single_line = redact_secrets(raw, secrets)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    truncate(&single_line, 140)
}

fn redact_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() >= 3 {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    let rows = value?.as_array()?;
    let choices: Vec<String> = rows
        .iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .filter(|item| !item.is_empty())
        .collect();
    if choices.is_empty() {
        None
    } else {
        Some(choices)
    }
}

fn path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("\n[truncated]");
    out
}

fn sanitize(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() >= 3 {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out = strip_urls(&out);
    let mut compact = String::new();
    let mut space = false;
    for ch in out.chars() {
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            if !space && !compact.is_empty() {
                compact.push(' ');
            }
            space = true;
        } else {
            compact.push(ch);
            space = false;
        }
    }
    truncate(compact.trim(), 240)
}

fn strip_urls(text: &str) -> String {
    const SCHEMES: [&str; 4] = ["https://", "http://", "wss://", "ws://"];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if let Some(scheme) = SCHEMES
            .iter()
            .copied()
            .find(|scheme| rest.starts_with(scheme))
        {
            out.push_str("[url]");
            let mut advanced = scheme.len();
            for next in rest[advanced..].chars() {
                if next.is_whitespace() {
                    break;
                }
                advanced += next.len_utf8();
            }
            rest = &rest[advanced..];
        } else {
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};

    // Allocation counter for the streaming hot-path budget test. Passes
    // every request through to the system allocator; counts only
    // allocations made on the measuring thread while armed, so parallel
    // tests and other threads never pollute the count.
    thread_local! {
        static ALLOC_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static ALLOC_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    struct CountingAlloc;
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOC_ARMED.with(|armed| {
                if armed.get() {
                    ALLOC_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            ALLOC_ARMED.with(|armed| {
                if armed.get() {
                    ALLOC_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOC_ARMED.with(|armed| {
                if armed.get() {
                    ALLOC_COUNT.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.realloc(ptr, layout, new_size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GLOBAL_ALLOC: CountingAlloc = CountingAlloc;

    /// Counts the allocations of polling one future to completion on this
    /// thread, resetting the counter first and disarming afterwards.
    async fn measure<F: Future>(f: F) -> (F::Output, usize) {
        ALLOC_COUNT.with(|count| count.set(0));
        ALLOC_ARMED.with(|armed| armed.set(true));
        let value = f.await;
        ALLOC_ARMED.with(|armed| armed.set(false));
        let count = ALLOC_COUNT.with(std::cell::Cell::get);
        (value, count)
    }

    #[test]
    fn steady_delta_frames_allocate_the_text_buffer_only() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            // The turn already exists: ensure_turn must not perturb the count.
            link.turns
                .insert("rt".into(), Turn::streaming("m1".into(), 1));
            while incoming.try_recv().is_ok() {}

            let frame = |offset: i64, text: &str| {
                format!(
                    r#"{{"method":"event","params":{{"type":"message.delta","session_id":"rt","payload":{{"turn_id":"m1","text":"{text}","offset":{offset}}}}}}}"#
                )
            };

            // First frame: cold pending insert — map key, runtime Arc,
            // message Arc, delta buffer.
            let cold_frame = frame(5, "hello");
            let (_, cold) = measure(link.on_text(&cold_frame)).await;
            assert_eq!(cold, 5, "cold insert: table + key + 2 id Arcs + tail");
            while incoming.try_recv().is_ok() {}

            // Steady keeping-up frames: the entry survives the successful
            // send, so only the delta buffer allocates. Before the Arc ids
            // and the surviving entry this frame cost seven allocations
            // (map key, entry id string, two event id strings, the tail,
            // the turn's message-id clone, and ensure_turn's unconditional
            // id rewrite).
            let steady_frame = frame(10, " more");
            let (_, steady_one) = measure(link.on_text(&steady_frame)).await;
            assert_eq!(steady_one, 1, "steady frame: buffer only");
            while incoming.try_recv().is_ok() {}
            let steady_frame = frame(15, " again");
            let (_, steady_two) = measure(link.on_text(&steady_frame)).await;
            assert_eq!(steady_two, 1, "steady frame: buffer only");
            while incoming.try_recv().is_ok() {}

            // A parked frame (channel full) allocates only the tail too:
            // ids are refcount clones, the entry already exists. (Sustained
            // parking restores the taken buffer on the failed send, so it
            // allocates nothing further once the tail has room.)
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("fill");
            let parked_frame = frame(25, "x");
            let (_, parked) = measure(link.on_text(&parked_frame)).await;
            assert_eq!(parked, 1, "parked frame: buffer only");
            assert!(link.pending_deltas.contains_key("rt"));
        });
    }

    #[test]
    fn late_ack_cannot_reopen_or_fail_newer_or_terminal_turn() {
        let mut newer = Turn::submitting("m".into(), 2);
        assert!(!newer.acknowledge(1));
        assert_eq!(newer.phase, TurnPhase::Submitting);
        assert!(!newer.fail_submit(1));
        assert!(newer.acknowledge(2));
        assert_eq!(newer.phase, TurnPhase::Streaming);
        assert!(!newer.fail_submit(2));
        assert!(newer.finish());
        assert!(!newer.acknowledge(2));
        assert!(!newer.fail_submit(2));
        assert!(!newer.fail_interrupt(2));
        assert!(!newer.accept_interrupt(1));
        assert!(!newer.note_accepted());
        assert_eq!(newer.phase, TurnPhase::Terminal);
        newer.note_withdraw();
        assert_eq!(newer.phase, TurnPhase::Terminal);

        let mut running = Turn::streaming("m".into(), 4);
        assert!(!running.acknowledge(4));
        assert!(running.accept_interrupt(4));
        assert_eq!(running.phase, TurnPhase::Stopping);
        assert_eq!(running.generation, 4);
        assert!(!running.fail_interrupt(4));
        assert!(!running.accept_interrupt(4));
        assert!(!running.acknowledge(4));
        assert!(running.note_accepted());
        assert_eq!(running.phase, TurnPhase::Stopping);
        assert!(running.finish());
        assert_eq!(running.phase, TurnPhase::Terminal);
        assert!(!running.accept_interrupt(4));
    }

    #[test]
    fn reconnect_pressed_mid_connect_aborts_and_retries_instead_of_waiting() {
        assert!(matches!(mid_connect(None), MidConnect::Shutdown));
        assert!(matches!(
            mid_connect(Some(Command::Shutdown)),
            MidConnect::Shutdown
        ));
        // The old behavior swallowed this press and let the attempt run to the
        // 90s connect bound; it must now restart the sign-in immediately.
        assert!(matches!(
            mid_connect(Some(Command::Reconnect)),
            MidConnect::Restart
        ));
        assert!(matches!(
            mid_connect(Some(Command::List { offset: 0 })),
            MidConnect::Queue(_)
        ));
        // The mid-dial queue must enforce the same allowlist the not-ready
        // path enforces: a Submit or Steer carries a runtime id from the
        // torn-down connection, so replaying it after the dial would target
        // dead state. It fails immediately instead.
        assert!(matches!(
            mid_connect(Some(Command::Submit {
                runtime_id: "rt-dead".into(),
                text: "hello".into(),
                attachments: Vec::new(),
            })),
            MidConnect::Reject(_)
        ));
        assert!(matches!(
            mid_connect(Some(Command::Steer {
                runtime_id: "rt-dead".into(),
                text: "redirect".into(),
            })),
            MidConnect::Reject(_)
        ));
        // Allowlisted commands still queue: they replay against fresh
        // state once the gateway reports ready.
        assert!(matches!(
            mid_connect(Some(Command::Resume {
                stored_id: "st-1".into()
            })),
            MidConnect::Queue(_)
        ));
    }

    #[test]
    fn mid_connect_rejects_emit_the_not_connected_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: "https://gateway.example".parse().expect("url"),
                    username: "user".into(),
                    password: "top-secret".into(),
                },
                events,
            )
            .expect("link");
            // No socket and not ready: a Submit is rejected the same way
            // open_link rejects one that arrives mid-dial.
            link.reject_unready(Command::Submit {
                runtime_id: "rt-dead".into(),
                text: "hello".into(),
                attachments: Vec::new(),
            })
            .await;
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::Error { message, .. })
                    if message.contains("Not connected. Reconnect to try again.")
            ));
            // A timeline command maps to its operation and stored id.
            link.reject_unready(Command::Undo {
                stored_id: "st-1".into(),
                runtime_id: "rt-dead".into(),
                mode: RewindMode::Undo,
                prompt_text: String::new(),
            })
            .await;
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::Error { operation, session_id, .. })
                    if operation == "timeline" && session_id == Some("st-1".into())
            ));
        });
    }

    #[test]
    fn keepalive_probe_fires_only_after_30s_of_silence() {
        let now = Instant::now();
        // Boundary: exactly PROBE_SILENCE is not yet due.
        assert!(!probe_due(now - Duration::from_secs(30), now));
        assert!(!probe_due(now - Duration::from_secs(29), now));
        assert!(!probe_due(now, now));
        assert!(probe_due(now - Duration::from_secs(31), now));
        assert!(probe_due(now - Duration::from_secs(90), now));
    }


    #[test]
    fn reconnect_backoff_doubles_then_caps_at_30s() {
        let schedule: Vec<Duration> = (1..=8).map(reconnect_delay).collect();
        assert_eq!(
            schedule,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn reconnect_deadline_resets_on_connect_and_never_retries_auth_failures() {
        let now = tokio::time::Instant::now();
        let mut attempts = 0;
        // Fixed jitter sample: half the 1s schedule is fixed, the scaled
        // half is 500ms * 0.5 = 250ms, so attempt 1 lands at exactly 750ms.
        let subsec = 500_000_000;

        // Live socket clears any pending retry and resets the backoff.
        assert_eq!(
            reconnect_deadline(
                true,
                false,
                Some(now + Duration::from_secs(9)),
                &mut attempts,
                now,
                subsec
            ),
            None
        );
        assert_eq!(attempts, 0);

        // A dropped, retryable socket schedules attempt 1 with jitter applied.
        let first = reconnect_deadline(false, true, None, &mut attempts, now, subsec).unwrap();
        assert_eq!(first, now + Duration::from_millis(750));
        assert_eq!(attempts, 1);

        // A pending retry is not rescheduled or shortened.
        assert_eq!(
            reconnect_deadline(false, true, Some(first), &mut attempts, now, subsec),
            Some(first)
        );
        assert_eq!(attempts, 1);

        // Authentication failure never schedules and does not grow attempts.
        assert_eq!(
            reconnect_deadline(false, false, None, &mut attempts, now, subsec),
            None
        );
        assert_eq!(attempts, 1);

        // After a successful reconnect the backoff starts over (jittered).
        reconnect_deadline(true, true, None, &mut attempts, now, subsec);
        let second = reconnect_deadline(false, true, None, &mut attempts, now, subsec).unwrap();
        assert_eq!(second, now + Duration::from_millis(750));
    }

    #[test]
    fn jittered_backoff_spreads_between_half_and_full_schedule() {
        // Bounds: zero nanos lands on half the schedule; nearly a full second
        // of nanos approaches the scheduled delay without exceeding it.
        assert_eq!(jittered(Duration::from_secs(1), 0), Duration::from_millis(500));
        assert_eq!(
            jittered(Duration::from_secs(1), 999_999_999),
            Duration::from_nanos(999_999_999)
        );
        // Monotonic in the sample, and never past the schedule.
        let mut previous = Duration::ZERO;
        for subsec in [0, 100_000_000, 500_000_000, 900_000_000, 999_999_999] {
            let delay = jittered(Duration::from_secs(30), subsec);
            assert!(delay >= previous);
            assert!(delay >= Duration::from_secs(15));
            assert!(delay <= Duration::from_secs(30));
            previous = delay;
        }
        // The lockstep case quantified: two clients that lost the same
        // connection, one sampling at each extreme, retry attempt 3 (4s)
        // 2s apart instead of on the same instant.
        assert_eq!(
            jittered(Duration::from_secs(4), 0),
            Duration::from_secs(2)
        );
        assert_eq!(
            jittered(Duration::from_secs(4), 999_999_999),
            Duration::from_nanos(3_999_999_998)
        );
    }

    #[test]
    fn delta_frames_parse_borrowed_without_the_json_dom() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");

            // A raw socket frame through on_text: the borrowed fast path.
            // Extra unknown fields ride along; turn_id/segment/offset are
            // absent and take the struct defaults, exactly like the DOM
            // fallback's unwrap_or values. An empty turn id adopts a
            // generated message id silently (no TurnRolled).
            link.on_text(
                r#"{"method":"event","params":{"type":"message.delta","session_id":"rt","payload":{"text":"hello world","vendor":"x"}}}"#,
            )
            .await;
            match incoming.try_recv().expect("delta") {
                Event::MessageDelta { runtime_id, delta, .. } => {
                    assert_eq!(&*runtime_id, "rt");
                    assert_eq!(delta, "hello world");
                }
                other => panic!("unexpected: {other:?}"),
            }

            // Steady state: the second frame reuses the entry (no key
            // allocation) and honors the accept-ordering offset.
            link.on_text(
                r#"{"method":"event","params":{"type":"message.delta","session_id":"rt","payload":{"text":" more","offset":11}}}"#,
            )
            .await;
            match incoming.try_recv().expect("second delta") {
                Event::MessageDelta { delta, .. } => assert_eq!(delta, " more"),
                other => panic!("unexpected: {other:?}"),
            }
            assert!(incoming.try_recv().is_err());

            // A frame carrying the needle but not the delta shape — text
            // missing — falls back and is ignored, no misroute, no panic.
            link.on_text(
                r#"{"method":"event","params":{"type":"message.delta","session_id":"rt","payload":{"text":null,"note":"mentions \"message.delta\" again"}}}"#,
            )
            .await;
            assert!(incoming.try_recv().is_err());
        });
    }

    #[test]
    fn needle_inside_a_tool_payload_does_not_take_the_delta_fast_path() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");

            // The needle appears inside a tool payload; the envelope check
            // must reject it and the DOM fallback must route the frame to
            // the tool handler, not the delta state machine.
            link.on_text(
                r#"{"method":"event","params":{"type":"tool.start","session_id":"rt","payload":{"tool":"grep","note":"watching \"message.delta\" frames"}}}"#,
            )
            .await;
            let mut saw_activity = false;
            while let Ok(event) = incoming.try_recv() {
                match event {
                    Event::MessageDelta { .. } => {
                        panic!("delta misrouted through the borrowed fast path")
                    }
                    Event::Activity { .. } => saw_activity = true,
                    _ => {}
                }
            }
            assert!(
                saw_activity,
                "tool frame must reach the tool handler"
            );
        });
    }

    #[test]
    fn clarify_answers_are_per_qid_and_preserve_locks() {
        let params = json!({
            "questions": [
                {"qid": "a", "question": "A?", "choices": ["yes", "no"], "multi_select": false},
                {"qid": "b", "question": "B?", "choices": ["red", "blue"], "multi_select": true},
                {"qid": "c", "question": "C?", "choices": null, "multi_select": false}
            ],
            "answers": {"a": "yes"}
        });
        let mut answers = HashMap::new();
        answers.insert("b".into(), Some("[\"red\",\"blue\"]".into()));
        answers.insert("c".into(), None);
        let result = interaction_result(
            "clarify",
            &params,
            &InteractionAnswer::Clarification(answers),
        )
        .unwrap();
        assert_eq!(result["answers"]["a"], json!("yes"));
        assert_eq!(result["answers"]["b"], json!("[\"red\",\"blue\"]"));
        assert!(result["answers"]["b"].is_string());
        assert!(result["answers"]["c"].is_null());
        assert!(result.get("outcome").is_none());

        let mut edited = HashMap::new();
        edited.insert("a".into(), Some("no".into()));
        edited.insert("b".into(), Some("red".into()));
        edited.insert("c".into(), None);
        assert!(
            interaction_result(
                "clarify",
                &params,
                &InteractionAnswer::Clarification(edited),
            )
            .is_err()
        );

        let mut missing = HashMap::new();
        missing.insert("b".into(), Some("red".into()));
        assert!(
            interaction_result(
                "clarify",
                &params,
                &InteractionAnswer::Clarification(missing),
            )
            .is_err()
        );

        let cancel = interaction_result("clarify", &params, &InteractionAnswer::Cancel).unwrap();
        assert!(cancel.get("answers").is_none());
        assert!(cancel.as_object().unwrap().is_empty());
    }

    #[test]
    fn resume_timeout_stays_retryable_and_method_refusal_is_readonly() {
        assert!(matches!(
            resume_disposition(-32000),
            ResumeDisposition::Retryable
        ));
        assert!(matches!(
            resume_disposition(-32601),
            ResumeDisposition::ReadOnly
        ));
        assert!(matches!(
            resume_disposition(4132),
            ResumeDisposition::ReadOnly
        ));
        assert!(matches!(
            resume_disposition(4007),
            ResumeDisposition::Missing
        ));
        assert!(matches!(
            resume_disposition(4023),
            ResumeDisposition::ReadOnly
        ));
        assert!(matches!(
            resume_disposition(0),
            ResumeDisposition::Retryable
        ));
    }

    #[test]
    fn history_pages_render_messages_and_summarize_tool_results() {
        let body = json!({
            "messages": [
                {"id": 1, "role": "user", "content": "first"},
                {"id": 2, "role": "tool", "tool_name": "terminal", "content": "{\"output\":\"private detail\"}"},
                {"id": 3, "role": "assistant", "content": "## Result\n\n**Done**"},
                {"id": 4, "role": "tool", "content": "unstructured details"}
            ],
            "pagination": {"order": "oldest", "returned": 4}
        });
        let rows = history_messages(&body, 4).unwrap();
        assert_eq!(rows[0].text, "first");
        assert_eq!(rows[1].role, Role::Tool);
        assert_eq!(rows[1].text, "Used terminal");
        assert_eq!(rows[2].text, "## Result\n\n**Done**");
        assert_eq!(rows[3].text, "Tool called");
        let latest = json!({
            "messages": [
                {"id": 1, "role": "assistant", "content": "second"},
                {"id": 2, "role": "user", "content": "first"}
            ],
            "pagination": {"order": "latest", "returned": 2}
        });
        assert!(history_messages(&latest, 2).is_err());
        assert!(
            history_messages(
                &json!({"messages": [{"id": 1, "role": "user", "content": "only"}]}),
                2
            )
            .is_err()
        );
    }

    #[test]
    fn session_list_rejects_missing_schema() {
        assert!(parse_sessions(&json!({}), 0).is_err());
        assert!(parse_sessions(&json!({"sessions": []}), 0).is_err());
        let empty = parse_sessions(&json!({"sessions": [], "total": 0}), 0).unwrap();
        match empty {
            Event::Sessions {
                sessions, has_more, ..
            } => {
                assert!(sessions.is_empty());
                assert!(!has_more);
            }
            _ => panic!("expected sessions"),
        }
    }

    #[test]
    fn server_request_ids_keep_json_types() {
        let numeric = rpc_result(&json!(12), json!({}));
        assert!(numeric["id"].is_number());
        assert_eq!(numeric["id"], json!(12));
        let text = rpc_error(&json!("srq-1"), -32601, "method not supported");
        assert!(text["id"].is_string());
        assert_eq!(text["id"], json!("srq-1"));
    }

    #[test]
    fn approval_defaults_do_not_offer_unlisted_permissions() {
        let absent = json!({
            "command": "rm -rf /tmp/very-long-secret-command",
            "description": "delete the scratch directory",
            "allow_session": true,
            "allow_permanent": true
        });
        assert_eq!(
            approval_choices(&absent),
            vec!["once".to_string(), "deny".to_string()]
        );
        let prompt = approval_prompt(&absent);
        assert!(prompt.contains("rm -rf /tmp/very-long-secret-command"));
        assert!(prompt.contains("delete the scratch directory"));
        assert_eq!(
            approval_choices(&json!({"choices": ["session", "deny"], "allow_permanent": true})),
            vec!["session".to_string(), "deny".to_string()]
        );
    }

    #[test]
    fn unhandled_notifications_do_not_fail_a_chat() {
        assert!(matches!(
            classify_unhandled_event("cron.changed"),
            UnhandledGatewayEvent::Ignore
        ));
        assert!(matches!(
            classify_unhandled_event("skills.changed"),
            UnhandledGatewayEvent::Ignore
        ));
        assert!(matches!(
            classify_unhandled_event("not.a.terminal.failure"),
            UnhandledGatewayEvent::Ignore
        ));
        assert!(matches!(
            classify_unhandled_event(""),
            UnhandledGatewayEvent::Malformed
        ));
    }

    #[test]
    fn strip_urls_keeps_unicode_boundaries_and_lowercase_schemes() {
        assert_eq!(
            strip_urls("café https://exämple.com/ä\u{00A0}wss://next ws://tail"),
            "café [url]\u{00A0}[url] [url]"
        );
        assert_eq!(strip_urls("prefixhttp://stay.example/x"), "prefix[url]");
        assert_eq!(strip_urls("HTTP://keep.example"), "HTTP://keep.example");
        assert_eq!(strip_urls("see https://x more"), "see [url] more");
    }

    #[test]
    fn gateway_error_finishes_the_active_turn_before_notifying() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.turns
                .insert("rt".into(), Turn::submitting("m".into(), 7));
            link.jobs.insert(
                1,
                Job {
                    kind: JobKind::Submit {
                        runtime_id: "rt".into(),
                        generation: 7,
                    },
                    deadline: Instant::now(),
                },
            );
            link.jobs.insert(
                2,
                Job {
                    kind: JobKind::Interrupt {
                        runtime_id: "rt".into(),
                        generation: 7,
                    },
                    deadline: Instant::now(),
                },
            );
            link.jobs.insert(
                3,
                Job {
                    kind: JobKind::Ping,
                    deadline: Instant::now(),
                },
            );
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "cron.changed",
                    "session_id": "rt",
                    "payload": {}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Submitting);
            assert!(link.jobs.contains_key(&1));
            assert!(incoming.try_recv().is_err());

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "error",
                    "session_id": "rt",
                    "payload": {"message": "boom https://exämple.com/secret"}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Terminal);
            assert!(link.turns["rt"].submit_settled);
            assert!(!link.jobs.contains_key(&1));
            assert!(!link.jobs.contains_key(&2));
            assert!(link.jobs.contains_key(&3));
            match incoming.try_recv().expect("gateway error") {
                Event::Error {
                    operation,
                    session_id,
                    message,
                } => {
                    assert_eq!(operation, "gateway");
                    assert_eq!(session_id.as_deref(), Some("rt"));
                    assert_eq!(message, "boom [url]");
                }
                other => panic!("unexpected event: {other:?}"),
            }

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "error",
                    "session_id": "rt",
                    "payload": {"message": "late"}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Terminal);
            assert!(link.jobs.contains_key(&3));
            assert!(incoming.try_recv().is_err());
        });
    }

    #[test]
    fn slow_ui_parks_deltas_instead_of_stalling_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");

            // Occupy the only slot: the bounded channel is full, so a delta
            // must park instead of stalling the loop on a blocking send.
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("fill");
            link.emit_delta("rt", "m1", "hello").await;
            assert_eq!(
                link.pending_deltas
                    .get("rt")
                    .map(|p| (p.message.as_ref(), p.tail.as_str())),
                Some(("m1", "hello"))
            );

            // A follow-up delta concatenates into the same parked tail.
            link.emit_delta("rt", "m1", " world").await;
            assert_eq!(
                link.pending_deltas.get("rt").map(|p| p.tail.as_str()),
                Some("hello world")
            );

            // A new message rolls the tail; with the channel full the old tail
            // is dropped rather than concatenated out of order.
            link.emit_delta("rt", "m2", "fresh").await;
            assert_eq!(
                link.pending_deltas
                    .get("rt")
                    .map(|p| (p.message.as_ref(), p.tail.as_str())),
                Some(("m2", "fresh"))
            );

            // With room again the parked tail hands over. The entry lingers
            // with an empty tail (ids are Arc clones, key reused); only
            // completion or a socket close clears the map.
            assert!(incoming.try_recv().is_ok());
            assert!(link.try_flush_delta("rt"));
            let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                panic!("parked delta must hand over on flush");
            };
            assert_eq!(delta, "fresh");
            assert!(link
                .pending_deltas
                .get("rt")
                .is_some_and(|p| p.tail.is_empty()));
            // An entry with an empty tail must not re-flush as a delta.
            assert!(!link.try_flush_delta("rt"));

            // A capped tail stays bounded and never splits a UTF-8 character.
            let big = "é".repeat(DELTA_TAIL_CAP / 2 + 40);
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("refill");
            link.emit_delta("rt", "m3", &big).await;
            let Some(entry) = link.pending_deltas.get("rt") else {
                panic!("oversized tail must park, not stall");
            };
            let tail = &entry.tail;
            assert!(tail.len() <= DELTA_TAIL_CAP + 3);
            assert!(std::str::from_utf8(tail.as_bytes()).is_ok());

            // A socket close drops parked tails with the rest of the live state;
            // the server's saved history is reloaded after reconnecting.
            link.close_socket().await;
            assert!(link.pending_deltas.is_empty());
        });
    }

    #[test]
    fn parked_deltas_resume_on_the_next_inbound_frame() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");

            // Park a tail behind a full channel, then let the UI catch up.
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("fill");
            link.emit_delta("rt", "m1", "hello").await;
            assert!(link.pending_deltas.contains_key("rt"));
            assert!(incoming.try_recv().is_ok());

            // The next inbound frame — not the 5s maintenance tick — hands
            // the parked tail over. Pong needs no write half, so this works
            // without a live socket.
            link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                Vec::new().into(),
            ))))
            .await;
            let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                panic!("frame arrival must resume parked tails");
            };
            assert_eq!(delta, "hello");
            // The flush drains the tail but the entry lingers (ids are Arc
            // clones; the map key is reused) so the next frame skips the
            // cold insert entirely.
            assert!(link
                .pending_deltas
                .get("rt")
                .is_some_and(|p| p.tail.is_empty()));
        });
    }

    #[test]
    fn slow_ui_parks_reasoning_instead_of_stalling_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");

            // Occupy the only slot: reasoning deltas park and concatenate
            // instead of stalling the loop on a blocking send.
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("fill");
            link.emit_reasoning_payload("rt", &json!({"text": "think"}), false);
            link.emit_reasoning_payload("rt", &json!({"text": " more"}), false);
            assert_eq!(
                link.pending_reasoning.get("rt"),
                Some(&(false, "think more".to_string()))
            );

            // An available summary replaces the parked tail wholesale.
            link.emit_reasoning_payload("rt", &json!({"text": "Summary."}), true);
            assert_eq!(
                link.pending_reasoning.get("rt"),
                Some(&(true, "Summary.".to_string()))
            );

            // With room again the next inbound frame hands the tail over,
            // replaying the replace flag.
            assert!(incoming.try_recv().is_ok());
            link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                Vec::new().into(),
            ))))
            .await;
            let Ok(Event::Reasoning { text, replace, .. }) = incoming.try_recv() else {
                panic!("parked reasoning must hand over on flush");
            };
            assert_eq!((text.as_str(), replace), ("Summary.", true));
            assert!(link.pending_reasoning.is_empty());

            // A turn roll drops the stale tail: the new turn's trace starts
            // clean instead of inheriting finished-turn reasoning.
            link.events
                .try_send(Event::Connection(ConnectionState::Connecting))
                .expect("refill");
            link.emit_reasoning_payload("rt", &json!({"text": "old turn"}), false);
            assert!(link.pending_reasoning.contains_key("rt"));
            link.turns.insert("rt".into(), Turn::streaming("m1".into(), 1));
            if let Some(turn) = link.turns.get_mut("rt") {
                turn.phase = TurnPhase::Terminal;
            }
            let _ = link.ensure_turn("rt", "m2");
            assert!(!link.pending_reasoning.contains_key("rt"));

            // A socket close drops parked reasoning with the rest of the
            // live state.
            link.emit_reasoning_payload("rt", &json!({"text": "tail"}), false);
            assert!(link.pending_reasoning.contains_key("rt"));
            link.close_socket().await;
            assert!(link.pending_reasoning.is_empty());
        });
    }

    /// A local HTTP server that reads each request and never replies: the
    /// client's own timeout is the only way out.
    async fn spawn_stalling_http() -> url::Url {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                std::future::pending::<()>().await;
            }
        });
        url::Url::parse(&format!("http://{addr}")).expect("origin")
    }

    /// A local HTTP server serving the workspace and skill catalogs by path.
    async fn spawn_catalog_http() -> url::Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("/api/skills") {
                    r#"[{"name":"b-plan","description":"Plan","category":"buck-workflow","enabled":true,"usage":3}]"#
                } else {
                    r#"{"default_cwd":"/home/alice/.hermes","home":"/home/alice","repos":[{"root":"/home/alice/app","label":"app","sessions":2}]}"#
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        url::Url::parse(&format!("http://{addr}")).expect("origin")
    }

    #[test]
    fn catalog_fetches_run_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // With the two catalog HTTP calls hung against a stalling
            // gateway, the launcher must return immediately and the
            // transport path must keep processing frames.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.fetch_studio_catalogs();
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while catalog HTTP hangs");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the spawned task still lands both
            // catalog events on the channel.
            let origin = spawn_catalog_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.fetch_studio_catalogs();
            let workspaces = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("workspaces within 5s")
                .expect("sender alive");
            assert!(matches!(
                &workspaces,
                Event::Workspaces { catalog }
                    if catalog.error.is_none() && catalog.options.len() == 2
            ));
            let skills = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("skills within 5s")
                .expect("sender alive");
            assert!(matches!(&skills, Event::Skills { skills } if skills.len() == 1));
        });
    }

    /// A local HTTP server serving the session list and message history by
    /// path, so the off-loop read tasks can be driven end to end.
    async fn spawn_sessions_http(sessions_body: &'static str) -> url::Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.contains("/api/sessions") && req.contains("/messages") {
                    r#"{"messages":[{"id":1,"role":"user","content":"first"},{"id":2,"role":"assistant","content":"second"}],"pagination":{"order":"oldest","returned":2}}"#
                } else if req.contains("/api/sessions") {
                    sessions_body
                } else {
                    "{}"
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        url::Url::parse(&format!("http://{addr}")).expect("origin")
    }

    #[test]
    fn session_reads_run_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // With the list and history HTTP calls hung against a stalling
            // gateway, the launchers must return immediately and the
            // transport path must keep processing frames.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.list(0);
                link.history("st-1".into(), PAGE);
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while list and history hang");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the fetched list re-enters the loop via
            // the work channel, where titles are cleaned and the listing
            // caches update; the history task emits directly.
            let origin = spawn_sessions_http(
                r#"{"total":1,"sessions":[{"id":"st-1","title":"Chat top-secret tail","source":"tui","message_count":3,"started_at":1794452594}]}"#,
            )
            .await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "top-secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.list(0);
            let listed = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("listed within 5s")
                .expect("sender alive");
            let StampedWork { work: LoopWork::Listed(listed), .. } = listed else {
                panic!("expected a fetched session list");
            };
            let Ok(event) = *listed else {
                panic!("expected a successful list fetch");
            };
            link.apply_listed(Ok(event)).await;
            let sessions = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("sessions within 5s")
                .expect("sender alive");
            assert!(matches!(
                &sessions,
                Event::Sessions { sessions, .. }
                    if sessions.len() == 1
                        && sessions[0].title == "Chat [REDACTED] tail"
                        && link.message_counts.get("st-1") == Some(&3)
            ));

            link.history("st-1".into(), 2);
            let history = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("history within 5s")
                .expect("sender alive");
            let Event::History { stored_id, messages, .. } = &history else {
                panic!("expected history, got {history:?}");
            };
            assert_eq!(
                (stored_id.as_str(), messages.len(), messages[0].text.as_str()),
                ("st-1", 2, "first")
            );
        });
    }

    #[test]
    fn stale_work_results_discarded_after_reconnect() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let origin = spawn_sessions_http(
                r#"{"total":1,"sessions":[{"id":"st-1","title":"Any","source":"tui","message_count":3,"started_at":1794452594}]}"#,
            )
            .await;
            let (events, _incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "top-secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");

            // While the connection is live, a fetched list re-enters the
            // loop and applies.
            link.list(0);
            let live = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("list within 5s")
                .expect("sender alive");
            assert!(link.live_work(live).is_some());

            // A teardown between spawn and delivery stales the result: a
            // resume fetched under the dropped connection would otherwise
            // adopt a turn for a runtime the next connection never resumed.
            link.list(0);
            link.close_socket().await;
            let stale = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("stale list within 5s")
                .expect("sender alive");
            assert_eq!(stale.epoch, 0);
            assert!(link.live_work(stale).is_none());

            // Work spawned after the teardown stamps the new epoch and
            // applies again.
            link.list(0);
            let fresh = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("fresh list within 5s")
                .expect("sender alive");
            assert_eq!(fresh.epoch, 1);
            assert!(link.live_work(fresh).is_some());
        });
    }

    #[test]
    fn image_uploads_run_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let png = vec![
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
            ];

            // With the image upload hung against a stalling gateway, the
            // submit launcher must return and frames must keep processing.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let img = StagedImage::from_bytes(1, "p.png", png.clone()).expect("staged");
                link.begin_submit("rt".into(), "with picture".into(), vec![img])
                    .await;
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while the upload hangs");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the upload result re-enters the loop via
            // the work channel; a failed upload fails the submit turn with
            // the same error the inline path produced.
            let origin = spawn_one_http_status("200 OK", "{}").await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            let img = StagedImage::from_bytes(1, "p.png", png).expect("staged");
            link.begin_submit("rt".into(), "with picture".into(), vec![img])
                .await;
            let uploaded = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("upload work within 5s")
                .expect("sender alive");
            let StampedWork {
                work: LoopWork::Uploaded {
                    runtime_id,
                    generation,
                    result,
                    prompt_text,
                },
                ..
            } = uploaded
            else {
                panic!("expected upload work");
            };
            assert!(result.is_err());
            link.apply_uploaded(runtime_id, generation, result, prompt_text)
                .await;
            let error = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("submit error within 5s")
                .expect("sender alive");
            assert!(matches!(
                &error,
                Event::Error { operation, .. } if operation == "submit"
            ));
        });
    }

    #[test]
    fn session_mutations_run_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // With the session patch hung against a stalling gateway, the
            // mutation launcher must return and frames must keep processing.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.mutate_session("st-1".into(), SessionMutation::SetPinned(true))
                    .await;
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while the patch hangs");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // A 404 for an unpersisted live conversation re-enters the loop
            // and defers the mutation, exactly as the inline path did.
            let origin = spawn_one_http_status("404 Not Found", "{}").await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.runtime_of.insert("st-new".into(), "rt-new".into());
            link.message_counts.insert("st-new".into(), 0);
            link.mutate_session("st-new".into(), SessionMutation::SetPinned(true))
                .await;
            let mutated =
                tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                    .await
                    .expect("mutation work within 5s")
                    .expect("sender alive");
            let StampedWork {
                work: LoopWork::Mutated {
                    stored_id,
                    mutation,
                    result,
                },
                ..
            } = mutated
            else {
                panic!("expected mutation work");
            };
            assert_eq!(stored_id, "st-new");
            assert!(matches!(result, Ok(PatchOutcome::NotFound)));
            link.apply_mutated(stored_id, mutation, result).await;
            let mutated_event =
                tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                    .await
                    .expect("session-mutated within 5s")
                    .expect("sender alive");
            assert!(matches!(
                &mutated_event,
                Event::SessionMutated { stored_id, .. } if stored_id == "st-new"
            ));
            assert!(link.deferred_mutations.contains_key("st-new"));
        });
    }

    #[test]
    fn branch_seeds_fetch_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let branch = BranchSeeds {
                stored_id: "st-1".into(),
                runtime_id: Some("rt".into()),
                parent_title: "Parent".into(),
                visible_count: Some(2),
                custom_title: None,
                seed_fallback: Vec::new(),
            };

            // With the earlier pages hung against a stalling gateway, the
            // branch launcher must return and frames must keep processing.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.begin_branch(
                    BranchSeeds {
                        stored_id: "st-1".into(),
                        runtime_id: Some("rt".into()),
                        parent_title: "Parent".into(),
                        visible_count: Some(2),
                        custom_title: None,
                        seed_fallback: Vec::new(),
                    },
                    40,
                )
                .await;
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while branch pages hang");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the fetched seeds re-enter the loop via
            // the work channel; without a socket the branch RPC fails with
            // the same branch error the inline path produced.
            let origin = spawn_sessions_http(
                r#"{"total":0,"sessions":[]}"#,
            )
            .await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.begin_branch(branch, 2).await;
            let branched =
                tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                    .await
                    .expect("branch work within 5s")
                    .expect("sender alive");
            let StampedWork { work: LoopWork::Branch(outcome), .. } = branched else {
                panic!("expected branch work");
            };
            let BranchSeedsOutcome { request, seeds } = *outcome;
            let earlier = seeds.expect("seeds must load from the serving origin");
            assert!(!earlier.is_empty());
            link.apply_branch(request, Ok(earlier)).await;
            let error = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("branch error within 5s")
                .expect("sender alive");
            assert!(matches!(
                &error,
                Event::Error { operation, .. } if operation == "branch"
            ));
        });
    }

    #[test]
    fn readonly_resume_loads_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // With the count walk and saved page hung against a stalling
            // gateway, the refused-resume launcher must return and frames
            // must keep processing.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.show_readonly("st-1".into(), "The gateway refused it.")
                    .await;
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while the transcript load hangs");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the transcript re-enters via the work
            // channel: the walked rows warm the listing caches, and the
            // same read-only Resumed event the inline path emitted lands.
            let origin = spawn_sessions_http(
                r#"{"total":1,"sessions":[{"id":"st-1","title":"Chat","source":"tui","message_count":2,"started_at":1794452594}]}"#,
            )
            .await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            assert!(link.refused.is_empty());
            link.show_readonly("st-1".into(), "The gateway refused it.")
                .await;
            assert!(link.refused.contains("st-1"));
            let readonly =
                tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                    .await
                    .expect("readonly work within 5s")
                    .expect("sender alive");
            let StampedWork { work: LoopWork::Readonly(outcome), .. } = readonly else {
                panic!("expected readonly work");
            };
            link.apply_readonly(*outcome).await;
            let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("resumed within 5s")
                .expect("sender alive");
            assert!(matches!(
                &resumed,
                Event::Resumed {
                    stored_id,
                    messages,
                    read_only_reason,
                    ..
                } if stored_id == "st-1"
                    && messages.len() == 2
                    && read_only_reason.as_deref() == Some("The gateway refused it.")
            ));
            assert_eq!(link.message_counts.get("st-1"), Some(&2));
        });
    }

    #[test]
    fn resume_pages_load_off_the_transport_loop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // With the saved page hung against a stalling gateway, the
            // resume handler must record the runtime mapping and count, and
            // frames must keep processing while the page loads.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(1);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                link.on_resumed(
                    "st-1".into(),
                    &json!({"session_id": "rt", "message_count": 40}),
                )
                .await;
                assert_eq!(link.runtime_of.get("st-1").map(String::as_str), Some("rt"));
                assert_eq!(link.message_counts.get("st-1"), Some(&40));
                link.events
                    .try_send(Event::Connection(ConnectionState::Connecting))
                    .expect("fill");
                link.emit_delta("rt", "m1", "hello").await;
                assert!(incoming.try_recv().is_ok());
                link.on_socket(Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(
                    Vec::new().into(),
                ))))
                .await;
                let Ok(Event::MessageDelta { delta, .. }) = incoming.try_recv() else {
                    panic!("frames must process while the resume page hangs");
                };
                assert_eq!(delta, "hello");
            })
            .await
            .expect("transport loop must stay responsive");

            // Against a live origin the page re-enters via the work channel
            // and the continuation replays: the Resumed event with the
            // runtime mapping and saved rows.
            let origin = spawn_sessions_http(r#"{"total":0,"sessions":[]}"#).await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.on_resumed(
                "st-1".into(),
                &json!({"session_id": "rt", "message_count": 2}),
            )
            .await;
            let resumed =
                tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                    .await
                    .expect("resume work within 5s")
                    .expect("sender alive");
            let StampedWork { work: LoopWork::Resumed(page), .. } = resumed else {
                panic!("expected resume work");
            };
            link.apply_resumed(*page).await;
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), incoming.recv())
                .await
                .expect("resumed event within 5s")
                .expect("sender alive");
            assert!(matches!(
                &event,
                Event::Resumed {
                    stored_id,
                    runtime_id,
                    messages,
                    read_only_reason,
                    ..
                } if stored_id == "st-1"
                    && runtime_id.as_deref() == Some("rt")
                    && messages.len() == 2
                    && read_only_reason.is_none()
            ));
        });
    }

    #[test]
    fn interrupt_ack_keeps_generation_until_message_complete() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(8);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.next_turn_generation = 4;
            link.turns
                .insert("rt".into(), Turn::streaming("m".into(), 4));
            link.jobs.insert(
                9,
                Job {
                    kind: JobKind::Submit {
                        runtime_id: "rt".into(),
                        generation: 4,
                    },
                    deadline: Instant::now(),
                },
            );
            link.on_job(
                Job {
                    kind: JobKind::Interrupt {
                        runtime_id: "rt".into(),
                        generation: 4,
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {}}),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert_eq!(link.turns["rt"].generation, 4);
            assert!(link.jobs.contains_key(&9));
            match incoming.try_recv().expect("interrupt ack") {
                Event::Interrupting { runtime_id } => assert_eq!(runtime_id, "rt"),
                other => panic!("ack must not be terminal: {other:?}"),
            }
            assert!(incoming.try_recv().is_err());
            link.on_server_request(
                "approval",
                &json!({
                    "jsonrpc": "2.0",
                    "id": "late-approval",
                    "method": "approval",
                    "params": {"session_id": "rt", "command": "echo"}
                }),
            )
            .await;
            link.on_server_request(
                "clarify",
                &json!({
                    "jsonrpc": "2.0",
                    "id": "late-clarify",
                    "method": "clarify",
                    "params": {
                        "session_id": "rt",
                        "questions": [{"qid": "q", "question": "Continue?"}]
                    }
                }),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert_eq!(link.turns["rt"].generation, 4);
            assert!(!link.asks.contains_key("s:late-approval"));
            assert!(!link.asks.contains_key("s:late-clarify"));
            assert!(link.asks.is_empty());
            while let Ok(event) = incoming.try_recv() {
                match event {
                    Event::Interaction { .. } | Event::InteractionCancelled { .. } => {
                        panic!("stopping must not adopt a late interaction: {event:?}")
                    }
                    Event::Error {
                        operation,
                        session_id,
                        ..
                    } => {
                        assert_eq!(operation, "interaction");
                        assert_eq!(session_id.as_deref(), Some("rt"));
                    }
                    other => panic!("late request must not resume the turn: {other:?}"),
                }
            }
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert_eq!(link.turns["rt"].generation, 4);
            link.asks.insert(
                "s:stale".into(),
                Ask {
                    method: "approval".into(),
                    params: json!({"choices": ["once", "deny"]}),
                    runtime_id: "rt".into(),
                    rpc_id: json!("stale-rpc"),
                },
            );
            link.respond(
                "rt".into(),
                "s:stale".into(),
                InteractionAnswer::Approval("once".into()),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert_eq!(link.turns["rt"].generation, 4);
            assert!(link.asks.is_empty());
            while let Ok(event) = incoming.try_recv() {
                match event {
                    Event::Error { operation, .. } => assert_eq!(operation, "interaction"),
                    other => panic!("stale approval must not continue the turn: {other:?}"),
                }
            }
            link.respond(
                "rt".into(),
                "s:stale".into(),
                InteractionAnswer::Approval("once".into()),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            match incoming.try_recv().expect("removed ask") {
                Event::Error { operation, .. } => assert_eq!(operation, "respond"),
                other => panic!("removed ask must stay closed: {other:?}"),
            }
            assert!(incoming.try_recv().is_err());

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.delta",
                    "session_id": "rt",
                    "payload": {"text": "partial", "offset": 0}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert_eq!(link.turns["rt"].generation, 4);
            match incoming.try_recv().expect("late delta") {
                Event::MessageDelta {
                    runtime_id, delta, ..
                } => {
                    assert_eq!(&*runtime_id, "rt");
                    assert_eq!(delta, "partial");
                }
                other => panic!("unexpected event: {other:?}"),
            }

            link.on_job(
                Job {
                    kind: JobKind::Submit {
                        runtime_id: "rt".into(),
                        generation: 4,
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {}}),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Stopping);
            assert!(incoming.try_recv().is_err());

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.complete",
                    "session_id": "rt",
                    "payload": {"status": "interrupted", "text": "partial"}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Terminal);
            assert_eq!(link.turns["rt"].generation, 4);
            assert!(!link.jobs.contains_key(&9));
            match incoming.try_recv().expect("terminal complete") {
                Event::MessageComplete {
                    runtime_id,
                    status,
                    message,
                } => {
                    assert_eq!(&*runtime_id, "rt");
                    assert_eq!(status, TurnStatus::Interrupted);
                    assert_eq!(message.text, "partial");
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert!(incoming.try_recv().is_err());

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.complete",
                    "session_id": "rt",
                    "payload": {"status": "interrupted", "text": "again"}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Terminal);
            assert!(incoming.try_recv().is_err());
        });
    }

    #[test]
    fn steer_routes_drop_only_while_stopping() {
        fn turn_with(phase: TurnPhase) -> Turn {
            let mut turn = Turn::streaming("m".into(), 1);
            turn.phase = phase;
            turn
        }
        assert!(matches!(steer_route(None), SteerRoute::Submit));
        for phase in [
            TurnPhase::Submitting,
            TurnPhase::Streaming,
            TurnPhase::AwaitingInput,
        ] {
            let turn = turn_with(phase);
            assert!(matches!(steer_route(Some(&turn)), SteerRoute::Redirect));
        }
        let mut stopping = turn_with(TurnPhase::Streaming);
        stopping.accept_interrupt(1);
        assert!(matches!(steer_route(Some(&stopping)), SteerRoute::Drop));
        let mut terminal = turn_with(TurnPhase::Streaming);
        terminal.finish();
        assert!(matches!(steer_route(Some(&terminal)), SteerRoute::Submit));
    }

    #[test]
    fn rejected_fallback_sends_queue_only_submit() {
        let params = queued_submit_params("rt", "hold on");
        assert_eq!(params["session_id"], "rt");
        assert_eq!(params["text"], "hold on");
        assert_eq!(params["queued"], true);
        let mut keys: Vec<&str> = params
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["queued", "session_id", "text"]);
    }

    #[test]
    fn steer_accept_events_leave_turn_and_generations_untouched() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.next_turn_generation = 4;
            link.turns
                .insert("rt".into(), Turn::streaming("m".into(), 4));
            link.jobs.insert(
                5,
                Job {
                    kind: JobKind::Submit {
                        runtime_id: "rt".into(),
                        generation: 4,
                    },
                    deadline: Instant::now(),
                },
            );
            link.on_job(
                Job {
                    kind: JobKind::Steer {
                        runtime_id: "rt".into(),
                        text: "go left".into(),
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {"status": "redirected"}}),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 4);
            assert!(link.jobs.contains_key(&5));
            match incoming.try_recv().expect("redirect accept") {
                Event::SteerAccepted {
                    runtime_id,
                    outcome,
                } => {
                    assert_eq!(&*runtime_id, "rt");
                    assert_eq!(outcome, SteerOutcome::Redirected);
                }
                other => panic!("unexpected event: {other:?}"),
            }

            link.on_job(
                Job {
                    kind: JobKind::Steer {
                        runtime_id: "rt".into(),
                        text: "and queue this".into(),
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {"status": "queued"}}),
            )
            .await;
            match incoming.try_recv().expect("queue accept") {
                Event::SteerAccepted { outcome, .. } => {
                    assert_eq!(outcome, SteerOutcome::Queued);
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 4);
        });
    }

    #[test]
    fn ws_dial_url_flips_scheme_sets_path_and_carries_only_the_ticket() {
        let origin = url::Url::parse("https://gw.example:8443").expect("origin");
        let url = ws_dial_url(&origin, "TK-1").expect("dial url");
        assert_eq!(url.scheme(), "wss");
        assert_eq!(url.host_str(), Some("gw.example"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.path(), "/api/ws");
        assert_eq!(url.query(), Some("ticket=TK-1"));

        let plain = url::Url::parse("http://gw.example").expect("origin");
        let url = ws_dial_url(&plain, "a b").expect("dial url");
        assert_eq!(url.scheme(), "ws");
        assert_eq!(url.query(), Some("ticket=a+b"));
    }

    #[test]
    fn fifo_followup_rolls_terminal_turn_and_ignores_late_interrupt() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.next_turn_generation = 7;
            let mut old = Turn::streaming("m1".into(), 7);
            old.finish();
            link.turns.insert("rt".into(), old);
            link.jobs.insert(
                8,
                Job {
                    kind: JobKind::Interrupt {
                        runtime_id: "rt".into(),
                        generation: 7,
                    },
                    deadline: Instant::now(),
                },
            );
            // Server-initiated follow-up: no client submit involved.
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.start",
                    "session_id": "rt",
                    "payload": {"turn_id": "m2"}
                }
            }))
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 8);
            assert_eq!(link.turns["rt"].message_id.as_ref(), "m2");
            assert_eq!(link.turns["rt"].segment, 0);
            assert_eq!(link.turns["rt"].applied_offset, 0);
            match incoming.try_recv().expect("rollover notice") {
                Event::TurnRolled { runtime_id } => assert_eq!(runtime_id, "rt"),
                other => panic!("unexpected event: {other:?}"),
            }

            // A late interrupt ack for the old generation must not reopen stopping.
            link.on_job(
                Job {
                    kind: JobKind::Interrupt {
                        runtime_id: "rt".into(),
                        generation: 7,
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {}}),
            )
            .await;
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 8);
            assert!(incoming.try_recv().is_err());

            // Deltas of the new turn route under its own message id.
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.delta",
                    "session_id": "rt",
                    "payload": {"turn_id": "m2", "text": "next turn", "offset": 0}
                }
            }))
            .await;
            match incoming.try_recv().expect("rolled delta") {
                Event::MessageDelta {
                    runtime_id,
                    message_id,
                    delta,
                } => {
                    assert_eq!(&*runtime_id, "rt");
                    assert_eq!(&*message_id, "m2");
                    assert_eq!(delta, "next turn");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });
    }

    #[test]
    fn delta_first_followup_rolls_an_untracked_turn_and_notifies() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.next_turn_generation = 3;
            // No tracked turn: a queued follow-up that survived a reconnect.
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.delta",
                    "session_id": "rt",
                    "payload": {"turn_id": "m9", "text": "first", "offset": 0}
                }
            }))
            .await;
            match incoming.try_recv().expect("rollover notice") {
                Event::TurnRolled { runtime_id } => assert_eq!(runtime_id, "rt"),
                other => panic!("unexpected event: {other:?}"),
            }
            match incoming.try_recv().expect("first delta") {
                Event::MessageDelta {
                    message_id, delta, ..
                } => {
                    assert_eq!(&*message_id, "m9");
                    assert_eq!(delta, "first");
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 4);
            assert_eq!(link.turns["rt"].message_id.as_ref(), "m9");
            // Continuation deltas of the same turn emit no further notice.
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "message.delta",
                    "session_id": "rt",
                    "payload": {"turn_id": "m9", "text": "more", "offset": 5}
                }
            }))
            .await;
            match incoming.try_recv().expect("second delta") {
                Event::MessageDelta { delta, .. } => assert_eq!(delta, "more"),
                other => panic!("unexpected event: {other:?}"),
            }
            assert!(incoming.try_recv().is_err());
        });
    }

    #[test]
    fn steer_failure_surfaces_without_ending_the_live_turn() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.next_turn_generation = 4;
            link.turns
                .insert("rt".into(), Turn::streaming("m".into(), 4));
            // No socket: the `rejected` fallback submit cannot be sent, which
            // exercises the steer failure path.
            link.on_job(
                Job {
                    kind: JobKind::Steer {
                        runtime_id: "rt".into(),
                        text: "steer".into(),
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {"status": "rejected"}}),
            )
            .await;
            match incoming.try_recv().expect("steer failure") {
                Event::Error {
                    operation,
                    session_id,
                    ..
                } => {
                    assert_eq!(operation, "steer");
                    assert_eq!(session_id.as_deref(), Some("rt"));
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert_eq!(link.turns["rt"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt"].generation, 4);
        });
    }

    #[test]
    fn session_title_event_maps_runtime_to_stored_session() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(4);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.runtime_of.insert("stored-42".into(), "rt-42".into());
            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "session.title",
                    "session_id": "rt-42",
                    "payload": {"title": "  Hyprland Command Palette  "}
                }
            }))
            .await;
            match incoming.try_recv().expect("title event") {
                Event::SessionTitle { session_id, title } => {
                    assert_eq!(session_id, "stored-42");
                    assert_eq!(title, "Hyprland Command Palette");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });
    }

    #[test]
    fn session_list_parses_float_timestamps_from_hermes_sqlite() {
        let parsed = parse_sessions(
            &json!({
                "total": 1,
                "sessions": [{
                    "id": "s-float",
                    "title": "Float epoch test",
                    "source": "tui",
                    "message_count": 12,
                    "started_at": 1_794_452_594.819_234
                }]
            }),
            0,
        )
        .expect("sessions");
        let Event::Sessions { sessions, .. } = parsed else {
            panic!("expected sessions");
        };
        assert_eq!(sessions[0].updated_at, "1794452594");
    }

    #[test]
    fn side_question_events_redact_secrets_and_never_interrupt_live_turns() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "alice".into(),
                    password: "top-secret-value".into(),
                },
                events,
            )
            .expect("link");
            link.turns
                .insert("rt-1".into(), Turn::streaming("msg-1".into(), 9));

            link.on_job(
                Job {
                    kind: JobKind::SideQuestion {
                        runtime_id: "rt-1".into(),
                        client_id: 7,
                    },
                    deadline: Instant::now(),
                },
                &json!({"result": {"task_id": "btw_abc"}}),
            )
            .await;
            match incoming.try_recv().expect("side started") {
                Event::SideStarted {
                    runtime_id,
                    client_id,
                    task_id,
                } => {
                    assert_eq!(runtime_id, "rt-1");
                    assert_eq!(client_id, 7);
                    assert_eq!(task_id, "btw_abc");
                }
                other => panic!("unexpected event: {other:?}"),
            }

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "btw.complete",
                    "session_id": "rt-1",
                    "payload": {
                        "task_id": "btw_abc",
                        "question": "Check secret",
                        "text": "Line 1\nSecret: top-secret-value"
                    }
                }
            }))
            .await;
            match incoming.try_recv().expect("side complete") {
                Event::SideComplete { task_id, text, .. } => {
                    assert_eq!(task_id, "btw_abc");
                    assert_eq!(text, "Line 1\nSecret: [REDACTED]");
                }
                other => panic!("unexpected event: {other:?}"),
            }

            // A side-question RPC error must emit SideFailed and leave the live turn Streaming:
            link.on_job(
                Job {
                    kind: JobKind::SideQuestion {
                        runtime_id: "rt-1".into(),
                        client_id: 8,
                    },
                    deadline: Instant::now(),
                },
                &json!({"error": {"code": -32601, "message": "method not found"}}),
            )
            .await;
            match incoming.try_recv().expect("side failed") {
                Event::SideFailed {
                    client_id, message, ..
                } => {
                    assert_eq!(client_id, 8);
                    assert!(message.contains("prompt.btw"));
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert_eq!(link.turns["rt-1"].phase, TurnPhase::Streaming);
            assert_eq!(link.turns["rt-1"].generation, 9);
        });
    }

    #[test]
    fn reasoning_tool_trace_and_usage_events_stream_into_flight_deck() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.turns
                .insert("rt-2".into(), Turn::streaming("msg-2".into(), 1));

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "reasoning.delta",
                    "session_id": "rt-2",
                    "payload": {"text": "Thinking about layout…"}
                }
            }))
            .await;
            assert!(matches!(incoming.try_recv().unwrap(), Event::Activity { .. }));
            match incoming.try_recv().expect("reasoning") {
                Event::Reasoning { text, replace, .. } => {
                    assert_eq!(text, "Thinking about layout…");
                    assert!(!replace);
                }
                other => panic!("unexpected: {other:?}"),
            }

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "tool.start",
                    "session_id": "rt-2",
                    "payload": {"tool_id": "t1", "name": "read_file", "preview": "src/ui.rs"}
                }
            }))
            .await;
            assert!(matches!(incoming.try_recv().unwrap(), Event::Activity { .. }));
            assert!(matches!(
                incoming.try_recv().unwrap(),
                Event::ToolTraceEvent {
                    update: ToolEventUpdate::Started { .. },
                    ..
                }
            ));

            link.on_event(&json!({
                "method": "event",
                "params": {
                    "type": "session.usage",
                    "session_id": "rt-2",
                    "payload": {"usage": {"input": 2000, "output": 400, "context_used": 2400, "context_max": 128000}}
                }
            }))
            .await;
            match incoming.try_recv().expect("telemetry") {
                Event::Telemetry { telemetry, .. } => {
                    assert_eq!(telemetry.input_tokens, 2000);
                    assert_eq!(telemetry.context_used, Some(2400));
                }
                other => panic!("unexpected: {other:?}"),
            }
        });
    }

    #[test]
    fn branch_and_undo_rpc_params_and_session_summary_parse_lineage() {
        assert_eq!(
            default_branch_title("Hyprland setup", None),
            "Hyprland setup (branch)"
        );
        assert_eq!(
            default_branch_title("Hyprland setup", Some("  Waybar fork  ")),
            "Waybar fork"
        );
        assert_eq!(
            branch_rpc_params("rt-1", None, None),
            json!({"session_id": "rt-1"})
        );
        assert_eq!(
            branch_rpc_params("rt-1", Some("Alt"), Some(2)),
            json!({"session_id": "rt-1", "name": "Alt", "count": 2})
        );
        let seeded = seeded_branch_params(
            "Alt",
            "parent-1",
            &[SeedMessage {
                role: "user".into(),
                content: "Hi".into(),
            }],
        );
        assert_eq!(seeded["title"], json!("Alt"));
        assert_eq!(seeded["parent_session_id"], json!("parent-1"));
        assert_eq!(seeded["messages"][0]["content"], json!("Hi"));
        assert_eq!(
            undo_rpc_params("rt-1", RewindMode::Undo),
            json!({"session_id": "rt-1", "intent": "undo"})
        );
        assert_eq!(
            undo_rpc_params("rt-1", RewindMode::EditRetry),
            json!({"session_id": "rt-1", "intent": "retry"})
        );
        assert_eq!(
            extract_session_ids(&json!({"session_id": "rt-9", "stored_session_id": "st-9"})),
            Some(("rt-9".into(), "st-9".into()))
        );
        let summary = session_summary(&json!({
            "id": "fork-1",
            "title": "Demo (branch)",
            "source": "tui",
            "message_count": 4,
            "parent_session_id": "root-1"
        }))
        .expect("summary");
        assert_eq!(summary.parent_session_id.as_deref(), Some("root-1"));
    }

    #[test]
    fn undo_empty_reply_and_4007_sync_settle_cleanly() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.on_undo_reply(
                "st-1".into(),
                "rt-1".into(),
                RewindMode::Undo,
                "prompt".into(),
                &json!({"removed": 0}),
            )
            .await;
            assert!(matches!(
                incoming.try_recv().expect("undo error"),
                Event::Error { ref operation, .. } if operation == "undo"
            ));

            link.fail_timeline_job(
                JobKind::UndoSync {
                    stored_id: "st-1".into(),
                    runtime_id: "rt-1".into(),
                    removed: 2,
                    mode: RewindMode::EditRetry,
                    prompt_text: "original".into(),
                },
                4007,
                "Session not found",
            )
            .await;
            match incoming.try_recv().expect("undone empty") {
                Event::Undone {
                    stored_id,
                    removed,
                    messages,
                    mode,
                    prompt_text,
                    ..
                } => {
                    assert_eq!(stored_id, "st-1");
                    assert_eq!(removed, 2);
                    assert!(messages.is_empty());
                    assert_eq!(mode, RewindMode::EditRetry);
                    assert_eq!(prompt_text, "original");
                }
                other => panic!("unexpected: {other:?}"),
            }
        });
    }

    #[test]
    fn timeline_branch_fallback_and_inline_branched_messages_emit_branched_event() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("http://127.0.0.1:9").expect("url"),
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            // Disconnected timeline commands emit clean non-fatal branch/undo errors.
            link.timeline_command(Command::Branch {
                stored_id: "root-1".into(),
                runtime_id: None,
                parent_title: "Root".into(),
                before_loaded: 0,
                visible_count: None,
                custom_title: None,
                seed_fallback: vec![SeedMessage {
                    role: "user".into(),
                    content: "Hi".into(),
                }],
            })
            .await;
            assert!(matches!(
                incoming.try_recv().expect("branch not connected"),
                Event::Error { ref operation, .. } if operation == "branch"
            ));

            // Inline branch reply falls back to result["messages"] when HTTP tail fetch fails.
            link.on_branched(
                "root-1".into(),
                "Root".into(),
                "Root (branch)".into(),
                1,
                &json!({
                    "session_id": "rt-fork",
                    "stored_session_id": "st-fork",
                    "message_count": 1,
                    "messages": [{"id": "m1", "role": "user", "content": "Seeded turn"}]
                }),
            )
            .await;
            let paged = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("branched page within 5s")
                .expect("sender alive");
            let StampedWork { work: LoopWork::Branched(page), .. } = paged else {
                panic!("expected branched page work");
            };
            link.apply_branched(*page).await;
            match incoming.try_recv().expect("branched") {
                Event::Branched {
                    session,
                    runtime_id,
                    messages,
                    ..
                } => {
                    assert_eq!(session.stored_id, "st-fork");
                    assert_eq!(session.title, "Root (branch)");
                    assert!(session.is_branch);
                    assert_eq!(runtime_id, "rt-fork");
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0].text, "Seeded turn");
                }
                other => panic!("unexpected: {other:?}"),
            }

            // Re-opened live branch swaps the transient branch runtime for the DB-backed resume runtime.
            let pending = link
                .build_pending_branch(
                    "root-1".into(),
                    "Root".into(),
                    "Root (branch)".into(),
                    1,
                    &json!({
                        "session_id": "rt-transient",
                        "stored_session_id": "st-reopened",
                        "title": "Custom fork",
                        "message_count": 1,
                        "messages": [{"id": "m1", "role": "user", "content": "Persisted turn"}]
                    }),
                )
                .expect("pending branch");
            link.finish_reopened_branch(
                pending,
                &json!({"session_id": "rt-resumed", "message_count": 1}),
            )
            .await;
            let reopened = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("reopened page within 5s")
                .expect("sender alive");
            let StampedWork { work: LoopWork::Branched(page), .. } = reopened else {
                panic!("expected reopened page work");
            };
            link.apply_branched(*page).await;
            assert_eq!(
                link.runtime_of.get("st-reopened").map(String::as_str),
                Some("rt-resumed")
            );
            match incoming.try_recv().expect("reopened branched") {
                Event::Branched {
                    session,
                    runtime_id,
                    messages,
                    ..
                } => {
                    assert_eq!(session.stored_id, "st-reopened");
                    assert_eq!(session.title, "Custom fork");
                    assert_eq!(runtime_id, "rt-resumed");
                    assert_eq!(messages.len(), 1);
                }
                other => panic!("unexpected: {other:?}"),
            }
        });
    }

    #[test]
    fn deferred_flush_refusal_re_defers_and_surfaces_instead_of_reverting() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // The gateway refuses the deferred save with 404 (and has no
            // mutation snapshot). The old fire-and-forget flush dropped this
            // outcome; the rename and pin silently reverted on the next
            // list refresh.
            let origin = spawn_one_http_status("404 Not Found", "{}").await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: origin,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.runtime_of.insert("st-live".into(), "rt-live".into());
            link.message_counts.insert("st-live".into(), 0);
            link.deferred_mutations.insert(
                "st-live".into(),
                (Some("Renamed".into()), Some(true)),
            );

            link.flush_deferred_for_runtime("rt-live");
            // The flush marks its own patch in flight; a user mutation for
            // the same conversation must ride the deferral, not race it.
            assert!(link.patching.contains("st-live"));
            assert!(!link.deferred_mutations.contains_key("st-live"));
            link.mutate_session("st-live".into(), SessionMutation::SetPinned(false))
                .await;
            assert!(link.deferred_mutations.contains_key("st-live"));
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::SessionMutated { stored_id, .. }) if stored_id == "st-live"
            ));

            let deferred =
                tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                    .await
                    .expect("deferred work within 5s")
                    .expect("sender alive");
            let StampedWork {
                work: LoopWork::Deferred {
                    stored_id,
                    title,
                    pinned,
                    result,
                },
                ..
            } = deferred
            else {
                panic!("expected deferred patch work");
            };
            assert_eq!(stored_id, "st-live");
            assert!(matches!(result, Ok(PatchOutcome::NotFound)));
            link.apply_deferred_patch(stored_id, title, pinned, result)
                .await;
            // The refusal re-defers the user's earlier intent and surfaces;
            // the absorbed SetPinned(false) rides on top of it.
            assert_eq!(
                link.deferred_mutations.get("st-live"),
                Some(&(Some("Renamed".into()), Some(false)))
            );
            assert!(!link.patching.contains("st-live"));
            match incoming.try_recv() {
                Ok(Event::Error { message, .. }) => assert!(
                    message.contains("Could not save conversation changes (HTTP 404)")
                ),
                other => panic!("unexpected: {other:?}"),
            }
        });
    }

    #[test]
    fn deferred_flush_in_flight_absorbs_user_mutations_without_racing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            // A stalling gateway keeps the in-flight patch unresolved.
            let stall = spawn_stalling_http().await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: stall,
                    username: "user".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("link");
            link.runtime_of.insert("st-live".into(), "rt-live".into());
            link.message_counts.insert("st-live".into(), 2);
            link.deferred_mutations.insert(
                "st-live".into(),
                (None, Some(true)),
            );

            link.flush_deferred_for_runtime("rt-live");
            assert!(link.patching.contains("st-live"));

            // The mutation pressed while the deferred flush is in flight is
            // absorbed into the deferral and shown as applied — no second
            // concurrent patch that would surface HTTP 409, no dropped
            // mutation. The pin is still in flight (the flush carries it),
            // so the map holds only the newer pending rename.
            link.mutate_session("st-live".into(), SessionMutation::Rename("Pinned fork".into()))
                .await;
            assert_eq!(
                link.deferred_mutations.get("st-live"),
                Some(&(Some("Pinned fork".into()), None))
            );
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::SessionMutated { stored_id, .. }) if stored_id == "st-live"
            ));
            // Still exactly one patch in flight.
            assert!(link.patching.contains("st-live"));

            // A teardown clears the in-flight marker: epoch-staled results
            // never reach the apply paths that would clear it.
            link.close_socket().await;
            assert!(link.patching.is_empty());
        });
    }

    #[test]
    fn session_summary_extracts_pinned_preview_and_model_fields() {
        let summary = session_summary(&json!({
            "id": "s-curated",
            "title": "Wayland compositor research",
            "source": "tui",
            "model": "claude-opus-4-6",
            "pinned": true,
            "preview": "  Investigated   layer-shell   anchoring  ",
            "message_count": 14,
            "last_active": 1794452594.0
        }))
        .expect("summary");
        assert!(summary.pinned);
        assert_eq!(summary.model, "claude-opus-4-6");
        assert_eq!(summary.preview, "Investigated layer-shell anchoring");
        assert_eq!(summary.message_count, 14);
    }

    #[test]
    fn unready_and_deferred_mutation_helpers_cover_curation_and_search() {
        assert!(is_queueable_while_connecting(&Command::SearchSessions {
            query: "hyprland".into(),
            request_seq: 1,
        }));
        assert!(!is_queueable_while_connecting(&Command::MutateSession {
            stored_id: "s1".into(),
            mutation: SessionMutation::Archive,
        }));
        assert_eq!(
            unready_session_failure(&Command::MutateSession {
                stored_id: "s1".into(),
                mutation: SessionMutation::SetPinned(true),
            }),
            Some(("curation", "s1".to_string()))
        );
        assert_eq!(
            deferred_patch_body(Some("Curated".into()), Some(true)),
            Some(json!({"title": "Curated", "pinned": true}))
        );
        assert_eq!(deferred_patch_body(None, None), None);
    }

    #[test]
    fn unpersisted_zero_turn_sessions_defer_curation_until_turn_or_archive() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: url::Url::parse("https://gateway.example").expect("url"),
                    username: "alice".into(),
                    password: "top-secret-value".into(),
                },
                events,
            )
            .expect("link");
            link.runtime_of.insert("st-new".into(), "rt-new".into());
            link.message_counts.insert("st-new".into(), 0);
            assert!(link.is_unpersisted_live("st-new"));

            link.apply_unpersisted_mutation("st-new".into(), SessionMutation::SetPinned(true))
                .await;
            link.apply_unpersisted_mutation(
                "st-new".into(),
                SessionMutation::Rename("Fresh".into()),
            )
            .await;
            assert_eq!(
                link.deferred_mutations.get("st-new"),
                Some(&(Some("Fresh".into()), Some(true)))
            );
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::SessionMutated { .. })
            ));
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::SessionMutated { .. })
            ));

            link.apply_unpersisted_mutation("st-new".into(), SessionMutation::Archive)
                .await;
            assert!(!link.deferred_mutations.contains_key("st-new"));
            assert!(!link.runtime_of.contains_key("st-new"));
        });
    }

    async fn spawn_one_http_status(status_line: &'static str, body: &'static str) -> url::Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let (st, bd) = if req.contains("/mutation-snapshot") {
                    ("404 Not Found", "{}")
                } else {
                    (status_line, body)
                };
                let reply = format!(
                    "HTTP/1.1 {st}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bd}",
                    bd.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        url::Url::parse(&format!("http://{addr}")).expect("origin")
    }

    #[test]
    fn archive_close_and_http_patch_outcomes_cover_conflict_and_completion() {
        let fenced = apply_mutation_snapshot(
            &json!({"pinned": true}),
            &json!({"exists": true, "runtime_revision": 7, "runtime_generation": "gen-2"}),
        );
        assert_eq!(fenced.get("expected_revision"), Some(&json!(7)));
        assert_eq!(fenced.get("expected_generation"), Some(&json!("gen-2")));
        assert!(
            fenced
                .get("request_id")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("hn-mut-"))
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let http = reqwest::Client::new();
            let ok_url = spawn_one_http_status("200 OK", "{}").await;
            assert_eq!(
                patch_session_http(&http, &ok_url, "s1", &json!({"pinned": true})).await,
                Ok(PatchOutcome::Applied)
            );
            let missing_url = spawn_one_http_status("404 Not Found", "{}").await;
            assert_eq!(
                patch_session_http(&http, &missing_url, "s1", &json!({"pinned": true})).await,
                Ok(PatchOutcome::NotFound)
            );
            let conflict_url = spawn_one_http_status("409 Conflict", "{}").await;
            assert_eq!(
                patch_session_http(&http, &conflict_url, "s1", &json!({"archived": true})).await,
                Ok(PatchOutcome::Conflict)
            );

            let archive_url = spawn_one_http_status("200 OK", "{}").await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: archive_url,
                    username: "alice".into(),
                    password: "top-secret-value".into(),
                },
                events,
            )
            .expect("link");
            let mut work_rx = link.work_rx.take().expect("work channel");
            link.on_curation_job(
                JobKind::ArchiveClose {
                    stored_id: "st-arc".into(),
                    retried: false,
                },
                &json!({"closed": true}),
            )
            .await;
            let archived = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("archive work within 5s")
                .expect("sender alive");
            let StampedWork {
                work: LoopWork::Archived {
                    stored_id,
                    retried,
                    result,
                },
                ..
            } = archived
            else {
                panic!("expected archive work");
            };
            link.apply_archived(stored_id, retried, result).await;
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::SessionMutated {
                    stored_id,
                    mutation: SessionMutation::Archive
                }) if stored_id == "st-arc"
            ));

            link.fail_curation_job(
                JobKind::ArchiveResume {
                    stored_id: "st-ro".into(),
                },
                4132,
                "active elsewhere",
            )
            .await;
            assert!(link.refused.contains("st-ro"));
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::Error { operation, .. }) if operation == "curation"
            ));
        });
    }

    #[test]
    fn studio_http_helpers_create_params_and_compress_noop_roundtrip() {
        assert_eq!(
            create_rpc_params("Work", Some(" /home/alice/repo ")),
            json!({"title": "Work", "cwd": "/home/alice/repo"})
        );
        assert_eq!(
            create_rpc_params("Work", Some("  ")),
            json!({"title": "Work"})
        );
        assert!(is_queueable_while_connecting(&Command::FetchStudio));
        assert_eq!(
            unready_session_failure(&Command::Compress {
                stored_id: "s-comp".into(),
                runtime_id: "rt-1".into()
            }),
            Some(("compress", "s-comp".into()))
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let http = reqwest::Client::new();
            let ws_url = spawn_one_http_status(
                "200 OK",
                r#"{"default_cwd":"/home/alice/.hermes","home":"/home/alice","repos":[{"root":"/home/alice/app","label":"app","sessions":2}]}"#,
            )
            .await;
            let cat = fetch_workspaces_http(&http, &ws_url, &[]).await.unwrap();
            assert_eq!(cat.options.len(), 2);

            let sk_url = spawn_one_http_status(
                "200 OK",
                r#"[{"name":"b-plan","description":"Plan","category":"buck-workflow","enabled":true,"usage":3}]"#,
            )
            .await;
            let skills = fetch_skills_http(&http, &sk_url, &[]).await.unwrap();
            assert_eq!(skills.len(), 1);

            let up_url = spawn_one_http_status(
                "200 OK",
                r#"{"ok":true,"path":"/home/alice/.hermes/cache/images/p.png"}"#,
            )
            .await;
            let png = vec![
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
            ];
            let img = StagedImage::from_bytes(1, "p.png", png).unwrap();
            let uploaded = upload_chat_image_http(&http, &up_url, &img).await.unwrap();
            assert_eq!(uploaded, "/home/alice/.hermes/cache/images/p.png");

            let base_url = spawn_one_http_status("200 OK", "{}").await;
            let (events, mut incoming) = tokio::sync::mpsc::channel(16);
            let mut link = Link::new(
                Config {
                    url: base_url,
                    username: "alice".into(),
                    password: "secret".into(),
                },
                events.clone(),
            )
            .expect("link");
            link.on_compress_reply(
                "st-1".into(),
                "rt-1".into(),
                &json!({"status": "compressed", "removed": 0, "summary": {"noop": true, "headline": "Already compact"}}),
            )
            .await;
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::Compressed { stored_id, messages: None, .. }) if stored_id == "st-1"
            ));

            let hist_url = spawn_one_http_status(
                "200 OK",
                r#"{"messages":[{"role":"system","content":"Compacted summary"},{"role":"assistant","content":"Ready"}]}"#,
            )
            .await;
            let mut sync_link = Link::new(
                Config {
                    url: hist_url,
                    username: "alice".into(),
                    password: "secret".into(),
                },
                events,
            )
            .expect("sync link");
            let summary = studio::parse_compress_summary(
                &json!({"status": "compressed", "removed": 4, "summary": {"noop": false, "headline": "Compacted 4 rows"}}),
                &[],
            );
            let mut work_rx = sync_link.work_rx.take().expect("work channel");
            sync_link
                .on_compress_sync_reply(
                    "st-2".into(),
                    "rt-2".into(),
                    summary,
                    &json!({"session_id": "rt-2b", "message_count": 2}),
                )
                .await;
            let paged = tokio::time::timeout(std::time::Duration::from_secs(5), work_rx.recv())
                .await
                .expect("compress page within 5s")
                .expect("sender alive");
            let StampedWork { work: LoopWork::Page(page), .. } = paged else {
                panic!("expected compress page work");
            };
            sync_link.apply_synced_page(*page).await;
            assert!(matches!(
                incoming.try_recv(),
                Ok(Event::Compressed { stored_id, runtime_id, messages: Some(rows), .. })
                    if stored_id == "st-2" && runtime_id == "rt-2b" && rows.len() == 2
            ));
        });
    }
}
