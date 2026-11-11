use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use crate::model::{
    Catalog, ChatMessage, ClarificationQuestion, Command, Config, ConnectionState, Event,
    InteractionAnswer, InteractionKind, InteractionRequest, ModelIdentity, Role, SessionSummary,
    TurnStatus,
};

const PAGE: usize = 50;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
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
    message_id: String,
    segment: i64,
    applied_offset: i64,
    generation: u64,
    phase: TurnPhase,
    submit_settled: bool,
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
    fn submitting(message_id: String, generation: u64) -> Self {
        Self {
            message_id,
            generation,
            phase: TurnPhase::Submitting,
            submit_settled: false,
            segment: 0,
            applied_offset: 0,
        }
    }

    fn streaming(message_id: String, generation: u64) -> Self {
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
    Interrupt {
        runtime_id: String,
        generation: u64,
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
    message_counts: HashMap<String, usize>,
    jobs: HashMap<u64, Job>,
    runtime_of: HashMap<String, String>,
    refused: HashSet<String>,
    asks: HashMap<String, Ask>,
    turns: HashMap<String, Turn>,
    activity: HashMap<String, String>,
    queued: VecDeque<Command>,
    heartbeat: bool,
    ready: bool,
    opened_at: Option<Instant>,
    last_inbound: Instant,
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
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    loop {
        let command = tokio::select! {
            biased;
            command = incoming.recv() => command,
            frame = next_frame(&mut link.read) => {
                link.on_socket(frame).await;
                continue;
            }
            _ = maintenance.tick() => {
                link.maintain().await;
                continue;
            }
        };
        let Some(command) = command else { break };
        if matches!(command, Command::Shutdown) {
            link.close_socket().await;
            break;
        }
        if matches!(command, Command::Reconnect) {
            if !open_link(&mut link, &mut incoming).await {
                break;
            }
            continue;
        }
        link.handle(command).await;
    }
    link.close_socket().await;
}

enum OpenStop {
    Shutdown,
    Ready,
    Failed,
    TimedOut,
}

async fn open_link(
    link: &mut Link,
    incoming: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> bool {
    let mut waiting = VecDeque::new();
    let stop = {
        let connection = link.connect();
        tokio::pin!(connection);
        let bound = tokio::time::sleep(CONNECT_TIMEOUT);
        tokio::pin!(bound);
        loop {
            tokio::select! {
                biased;
                command = incoming.recv() => match command {
                    None | Some(Command::Shutdown) => break OpenStop::Shutdown,
                    Some(Command::Reconnect) => {},
                    Some(command) => waiting.push_back(command),
                },
                result = &mut connection => break match result {
                    Ok(()) => OpenStop::Ready,
                    Err(_) => OpenStop::Failed,
                },
                _ = &mut bound => break OpenStop::TimedOut,
            }
        }
    };
    if matches!(stop, OpenStop::TimedOut) {
        link.close_socket().await;
        link.emit(Event::Connection(ConnectionState::Disconnected(
            "The gateway connection timed out.".into(),
        )))
        .await;
    }
    link.queued.extend(waiting);
    !matches!(stop, OpenStop::Shutdown)
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
            message_counts: HashMap::new(),
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

    async fn connect(&mut self) -> Result<(), String> {
        self.emit(Event::Connection(ConnectionState::Connecting))
            .await;
        self.ready = false;
        self.opened_at = None;
        self.close_socket().await;
        self.fail_jobs("The connection closed before the gateway answered.")
            .await;
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
        if !login.status().is_success() {
            let state = if login.status().as_u16() == 401 || login.status().as_u16() == 404 {
                ConnectionState::AuthenticationFailed(
                    "The gateway rejected the username or password.".into(),
                )
            } else if login.status().as_u16() == 429 {
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
            return Err("login failed".into());
        }
        let ticket_response = match self
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
        if ticket_response.status().as_u16() == 401 {
            self.emit(Event::Connection(ConnectionState::AuthenticationFailed(
                "The gateway did not accept the login session.".into(),
            )))
            .await;
            return Err("ticket unauthorized".into());
        }
        if !ticket_response.status().is_success() {
            self.emit(Event::Connection(ConnectionState::Disconnected(format!(
                "Could not request a connection ticket (HTTP {}).",
                ticket_response.status().as_u16()
            ))))
            .await;
            return Err("ticket failed".into());
        }
        let ticket_body: Value = match ticket_response.json().await {
            Ok(body) => body,
            Err(_) => {
                self.emit(Event::Connection(ConnectionState::Disconnected(
                    "The gateway ticket response was not usable.".into(),
                )))
                .await;
                return Err("ticket body".into());
            }
        };
        let Some(ticket) = ticket_body
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            self.emit(Event::Connection(ConnectionState::Disconnected(
                "The gateway did not provide a connection ticket.".into(),
            )))
            .await;
            return Err("missing ticket".into());
        };
        self.secrets.truncate(2);
        self.secrets.push(ticket.clone());
        let mut ws_url = self.origin.clone();
        let scheme = if self.origin.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        if ws_url.set_scheme(scheme).is_err() {
            self.emit(Event::Connection(ConnectionState::Disconnected(
                "Could not open the gateway socket.".into(),
            )))
            .await;
            return Err("scheme".into());
        }
        ws_url.set_path("/api/ws");
        ws_url
            .query_pairs_mut()
            .clear()
            .append_pair("ticket", &ticket);
        let url_string: String = ws_url.into();
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
        while self.ready && self.write.is_some() {
            let Some(command) = self.queued.pop_front() else {
                break;
            };
            self.handle(command).await;
            if !self.ready || self.write.is_none() {
                break;
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        if (!self.ready || self.write.is_none())
            && !matches!(command, Command::Reconnect | Command::Shutdown)
        {
            if self.write.is_some()
                || matches!(
                    command,
                    Command::List { .. } | Command::Create { .. } | Command::Resume { .. }
                )
            {
                self.queued.push_back(command);
                return;
            }
            self.fail("connect", None, "Not connected. Reconnect to try again.")
                .await;
            return;
        }
        match command {
            Command::Reconnect => {} // The supervisor handles connection cancellation.
            Command::Shutdown => {}
            Command::List { offset } => self.list(offset).await,
            Command::Create { title } => {
                if let Err(fault) = self
                    .send_rpc(
                        "session.create",
                        json!({"title": title}),
                        JobKind::Create { title },
                        RPC_TIMEOUT,
                    )
                    .await
                {
                    self.fail("create", None, send_fault_message("create", fault))
                        .await;
                }
            }
            Command::Resume { stored_id } => {
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
            Command::History { stored_id, before } => self.history(stored_id, before).await,
            Command::Submit { runtime_id, text } => {
                if self
                    .turns
                    .get(&runtime_id)
                    .is_some_and(|turn| turn.phase == TurnPhase::Stopping)
                {
                    return;
                }
                self.next_turn_generation += 1;
                let generation = self.next_turn_generation;
                self.turns.insert(
                    runtime_id.clone(),
                    Turn::submitting(format!("turn-{runtime_id}-{generation}"), generation),
                );
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
                    if let Some(turn) = self.turns.get_mut(&runtime_id) {
                        let _ = turn.fail_submit(generation);
                    }
                    self.fail(
                        "submit",
                        Some(runtime_id),
                        send_fault_message("submit", fault),
                    )
                    .await;
                }
            }
            Command::Interrupt { runtime_id } => {
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
            Command::Respond {
                runtime_id,
                request_id,
                answer,
            } => self.respond(runtime_id, request_id, answer).await,
            Command::Delete { stored_id } => self.begin_delete(stored_id).await,
            Command::FetchModels | Command::SetModel { .. } => {
                self.model_command(command).await;
            }
        }
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
            _ => {}
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

    async fn list(&mut self, offset: usize) {
        match fetch_sessions(&self.http, &self.origin, offset).await {
            Ok(event) => {
                if let Event::Sessions { sessions, .. } = &event {
                    for session in sessions {
                        self.note_listed(session);
                    }
                }
                self.emit(event).await;
            }
            Err(message) => self.fail("list", None, message).await,
        }
    }

    async fn history(&self, stored_id: String, before: usize) {
        let next_before = before.saturating_sub(PAGE);
        match fetch_history(
            &self.http,
            &self.origin,
            &stored_id,
            next_before,
            before - next_before,
        )
        .await
        {
            Ok(messages) => {
                self.emit(Event::History {
                    stored_id,
                    messages,
                    before,
                    next_before,
                })
                .await
            }
            Err(message) => self.fail("history", Some(stored_id), message).await,
        }
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
            tokio::time::timeout(RPC_TIMEOUT, write.send(Message::text(text))).await,
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
        match frame {
            Message::Text(text) => self.on_text(text.as_str()).await,
            Message::Ping(payload) => {
                let failed = if let Some(write) = self.write.as_mut() {
                    !matches!(
                        tokio::time::timeout(RPC_TIMEOUT, write.send(Message::Pong(payload))).await,
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
            "message.start" => {
                let message_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("turn-{runtime_id}"));
                let turn = self.ensure_turn(&runtime_id, &message_id);
                if turn.note_accepted() {
                    self.retire_submit_jobs(&runtime_id);
                }
            }
            "message.delta" => self.on_delta(&runtime_id, payload).await,
            "message.complete" => self.on_complete(&runtime_id, payload).await,
            "thinking.delta" | "reasoning.delta" => {
                self.activity(&runtime_id, "Hermes is thinking…").await
            }
            "tool.start" | "tool.generating" | "tool.complete" => {
                let name = payload
                    .get("name")
                    .or_else(|| payload.get("tool"))
                    .or_else(|| payload.get("tool_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let label = match event_type {
                    "tool.complete" => format!("Tool finished: {}", truncate(name, 80)),
                    _ => format!("Using tool: {}", truncate(name, 80)),
                };
                self.activity(&runtime_id, &label).await;
            }
            "request.cancel" => {
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
            "error" => {
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
            "session.info" | "session.title" | "sessions.changed" | "platforms.changed"
            | "message.interim" => {}
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

    async fn on_delta(&mut self, runtime_id: &str, payload: &Value) {
        let Some(text) = payload.get("text").and_then(Value::as_str) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let turn_id = payload.get("turn_id").and_then(Value::as_str).unwrap_or("");
        let segment = payload.get("segment").and_then(Value::as_i64).unwrap_or(0);
        let offset = payload.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let turn = self.ensure_turn(runtime_id, turn_id);
        if !turn.note_accepted() {
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
        self.emit(Event::MessageDelta {
            runtime_id: runtime_id.to_string(),
            message_id,
            delta: text.to_owned(),
        })
        .await;
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
        let message_id = self.ensure_turn(runtime_id, "").message_id.clone();
        if let Some(turn) = self.turns.get_mut(runtime_id) {
            let _ = turn.finish();
        }
        self.retire_turn_jobs(runtime_id);
        self.activity.remove(runtime_id);
        self.emit(Event::MessageComplete {
            runtime_id: runtime_id.to_string(),
            message: ChatMessage {
                id: message_id,
                role: Role::Assistant,
                text: text.to_owned(),
            },
            status,
        })
        .await;
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
    }

    fn ensure_turn(&mut self, runtime_id: &str, message_id: &str) -> &mut Turn {
        if !self.turns.contains_key(runtime_id) {
            self.next_turn_generation += 1;
            let id = if message_id.is_empty() {
                format!("turn-{runtime_id}-{}", self.next_turn_generation)
            } else {
                message_id.to_string()
            };
            self.turns.insert(
                runtime_id.to_string(),
                Turn::streaming(id, self.next_turn_generation),
            );
        }
        let turn = self.turns.get_mut(runtime_id).expect("inserted turn");
        if !message_id.is_empty() && turn.phase != TurnPhase::Terminal {
            turn.message_id = message_id.to_string();
        }
        turn
    }

    fn retire_submit_jobs(&mut self, runtime_id: &str) {
        self.jobs.retain(|_, job| {
            !matches!(&job.kind, JobKind::Submit { runtime_id: id, .. } if id == runtime_id)
        });
    }

    fn retire_turn_jobs(&mut self, runtime_id: &str) {
        self.jobs.retain(|_, job| {
            !matches!(
                &job.kind,
                JobKind::Submit { runtime_id: id, .. } | JobKind::Interrupt { runtime_id: id, .. } if id == runtime_id
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
            self.ensure_turn(&runtime_id, "").await_input();
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
            } => {
                if self
                    .turns
                    .get_mut(&runtime_id)
                    .is_some_and(|turn| turn.acknowledge(generation))
                {
                    self.emit(Event::Submitted { runtime_id }).await;
                }
            }
            JobKind::Interrupt {
                runtime_id,
                generation,
            } => {
                let accepted = self
                    .turns
                    .get_mut(&runtime_id)
                    .is_some_and(|turn| turn.accept_interrupt(generation));
                if accepted {
                    self.emit(Event::Interrupting { runtime_id }).await;
                }
            }
            JobKind::Catalog => match Catalog::parse(result) {
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
            },
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
            JobKind::Delete { stored_id, .. } => {
                self.runtime_of.remove(&stored_id);
                self.emit(Event::Deleted { stored_id }).await;
            }
            JobKind::DeleteResume { stored_id } => {
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
        }
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
            } => {
                let applies = self
                    .turns
                    .get_mut(&runtime_id)
                    .is_some_and(|turn| turn.fail_submit(generation));
                if !applies {
                    return;
                }
                self.fail(
                    "submit",
                    Some(runtime_id),
                    format!("{message} The prompt will not be sent again."),
                )
                .await
            }
            JobKind::Interrupt {
                runtime_id,
                generation,
            } => {
                if !self
                    .turns
                    .get(&runtime_id)
                    .is_some_and(|turn| turn.fail_interrupt(generation))
                {
                    return;
                }
                self.fail("interrupt", Some(runtime_id), message).await
            }
            kind @ (JobKind::Catalog | JobKind::Prime { .. } | JobKind::Switch { .. }) => {
                self.fail_model_request(kind, code, message).await;
            }
            JobKind::Close { stored_id, retried } => self.request_delete(stored_id, retried).await,
            JobKind::DeleteResume { stored_id } => {
                if matches!(resume_disposition(code), ResumeDisposition::ReadOnly) {
                    self.refused.insert(stored_id.clone());
                }
                self.fail("delete", Some(stored_id), message).await
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
        self.emit(Event::Created {
            session: SessionSummary {
                stored_id,
                title,
                source: "tui".into(),
                updated_at: String::new(),
                read_only_reason: None,
                message_count: 0,
            },
            runtime_id,
            model,
        })
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
        let running = result
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || matches!(
                result.get("status").and_then(Value::as_str),
                Some("streaming" | "running" | "resuming")
            )
            || result
                .get("inflight")
                .is_some_and(|value| value.is_object());
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
        let messages =
            match fetch_history(&self.http, &self.origin, &stored_id, before, count - before).await
            {
                Ok(messages) => messages,
                Err(message) => {
                    self.fail("resume", Some(stored_id), message).await;
                    return;
                }
            };
        self.emit(Event::Resumed {
            stored_id,
            runtime_id: runtime_id.clone(),
            messages,
            before,
            read_only_reason: None,
            model: model_identity(result),
        })
        .await;
        if running && let Some(runtime_id) = runtime_id.clone() {
            self.next_turn_generation += 1;
            let generation = self.next_turn_generation;
            self.turns.insert(
                runtime_id.clone(),
                Turn::streaming(format!("turn-{runtime_id}-{generation}"), generation),
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
        if session.source == "kanban" {
            self.refused.insert(session.stored_id.clone());
        }
    }

    async fn show_readonly(&mut self, stored_id: String, message: &str) {
        self.refused.insert(stored_id.clone());
        let reason = self.clean(message.to_string());
        let count = match self.history_count(&stored_id).await {
            Ok(count) => count,
            Err(error) => {
                self.fail(
                    "resume",
                    Some(stored_id),
                    format!("{reason} Saved history could not be loaded: {error}"),
                )
                .await;
                return;
            }
        };
        let before = count.saturating_sub(PAGE);
        let messages =
            match fetch_history(&self.http, &self.origin, &stored_id, before, count - before).await
            {
                Ok(messages) => messages,
                Err(error) => {
                    self.fail(
                        "resume",
                        Some(stored_id),
                        format!("{reason} Saved history could not be loaded: {error}"),
                    )
                    .await;
                    return;
                }
            };
        self.emit(Event::Resumed {
            stored_id,
            runtime_id: None,
            messages,
            before,
            read_only_reason: Some(if reason.is_empty() {
                "This conversation cannot be continued. Showing the saved transcript.".into()
            } else {
                reason
            }),
            model: None,
        })
        .await;
    }

    async fn history_count(&mut self, stored_id: &str) -> Result<usize, String> {
        if let Some(count) = self.message_counts.get(stored_id) {
            return Ok(*count);
        }
        let mut offset = 0;
        loop {
            let Event::Sessions {
                sessions, has_more, ..
            } = fetch_sessions(&self.http, &self.origin, offset).await?
            else {
                return Err("The conversation list was not usable.".into());
            };
            for session in &sessions {
                self.note_listed(session);
            }
            if let Some(count) = self.message_counts.get(stored_id) {
                return Ok(*count);
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

    async fn maintain(&mut self) {
        let now = Instant::now();
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
        if self.write.is_some()
            && !self.ready
            && self
                .opened_at
                .is_some_and(|opened| opened.elapsed() > READY_TIMEOUT)
        {
            self.drop_socket("The gateway did not become ready.").await;
            return;
        }
        if self.write.is_some() && self.last_inbound.elapsed() > Duration::from_secs(90) {
            self.drop_socket("The gateway stopped responding.").await;
            return;
        }
        if self.write.is_some()
            && self.heartbeat
            && self.last_inbound.elapsed() > Duration::from_secs(20)
        {
            let _ = self
                .send_rpc("gateway.ping", json!({}), JobKind::Ping, RPC_TIMEOUT)
                .await;
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
        self.runtime_of.clear();
    }
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

fn session_summary(row: &Value) -> Result<SessionSummary, String> {
    let stored_id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "The conversation list was not usable.".to_string())?
        .to_string();
    let count = row
        .get("message_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "The conversation list was not usable.".to_string())?;
    let message_count =
        usize::try_from(count).map_err(|_| "The conversation list was not usable.".to_string())?;
    let title = row
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let updated = row
        .get("last_active")
        .or_else(|| row.get("last_activity_at"))
        .or_else(|| row.get("started_at"))
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .unwrap_or_default();
    Ok(SessionSummary {
        stored_id,
        title: if title.is_empty() {
            "Untitled".into()
        } else {
            title.to_string()
        },
        source: row
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        updated_at: updated,
        read_only_reason: None,
        message_count,
    })
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
                    assert_eq!(runtime_id, "rt");
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
                    assert_eq!(runtime_id, "rt");
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
}
