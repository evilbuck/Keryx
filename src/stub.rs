//! Loopback Hermes gateway for `--preview`.
//!
//! Speaks the slice of the dashboard the desk uses: password login, a websocket
//! ticket, session pages, history pages, and the JSON-RPC methods the client
//! sends after `gateway.ready`. Nothing here is a real Hermes.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

const NOTES: &str = "notes";
const BOARD: &str = "board";

pub fn start() -> Result<u16, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hermes-preview".into())
        .spawn(move || serve(sender))
        .map_err(|_| "Could not start the preview gateway.".to_string())?;
    receiver
        .recv()
        .map_err(|_| "The preview gateway stopped before it bound.".to_string())?
}

fn serve(sender: std::sync::mpsc::Sender<Result<u16, String>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = sender.send(Err("Could not start the preview runtime.".into()));
            return;
        }
    };
    runtime.block_on(bind_and_accept(sender));
}

async fn bind_and_accept(sender: std::sync::mpsc::Sender<Result<u16, String>>) {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(_) => {
            let _ = sender.send(Err("Could not bind the preview gateway.".into()));
            return;
        }
    };
    match listener.local_addr() {
        Ok(addr) => {
            let _ = sender.send(Ok(addr.port()));
        }
        Err(_) => {
            let _ = sender.send(Err("Could not read the preview gateway port.".into()));
            return;
        }
    }
    accept_loop(listener).await;
}

async fn accept_loop(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle(stream));
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

async fn handle(mut stream: TcpStream) {
    if websocket_upgrade(&stream).await {
        if let Ok(socket) = tokio_tungstenite::accept_async(stream).await {
            talk(socket).await;
        }
        return;
    }
    let request = read_http(&mut stream).await;
    let (status, reason, body, cookie) = route(&request);
    let _ = stream
        .write_all(response(status, reason, &body, cookie).as_bytes())
        .await;
}

async fn websocket_upgrade(stream: &TcpStream) -> bool {
    let header = peek_header(stream).await;
    let text = String::from_utf8_lossy(&header).to_ascii_lowercase();
    text.contains("upgrade: websocket")
}

async fn peek_header(stream: &TcpStream) -> Vec<u8> {
    let mut buf = vec![0u8; 8192];
    let start = std::time::Instant::now();
    loop {
        let Ok(read) = stream.peek(&mut buf).await else {
            return Vec::new();
        };
        if header_end(&buf[..read]).is_some() || start.elapsed() > Duration::from_secs(2) {
            buf.truncate(read);
            return buf;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn read_http(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(read) = stream.read(&mut buf).await {
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buf[..read]);
        if complete_http(&data) || data.len() > 1_048_576 {
            break;
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn complete_http(data: &[u8]) -> bool {
    let Some(end) = header_end(data) else {
        return false;
    };
    data.len() >= end + 4 + content_length(data)
}

fn header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(data: &[u8]) -> usize {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        let Some(value) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        else {
            continue;
        };
        return value.trim().parse().unwrap_or(0);
    }
    0
}

fn route(request: &str) -> (u16, &'static str, String, bool) {
    let Some(line) = request.lines().next() else {
        return (400, "Bad Request", json_error("bad request"), false);
    };
    let (method, path, query) = request_target(line);
    post_route(method, path)
        .or_else(|| get_route(method, path, query))
        .unwrap_or((404, "Not Found", json_error("not found"), false))
}

fn request_target(line: &str) -> (&str, &str, &str) {
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (method, path, query)
}

fn post_route(method: &str, path: &str) -> Option<(u16, &'static str, String, bool)> {
    if method != "POST" {
        return None;
    }
    match path {
        "/auth/password-login" => Some((200, "OK", "{}".into(), true)),
        "/api/auth/ws-ticket" => Some((200, "OK", "{\"ticket\":\"preview\"}".into(), false)),
        _ => None,
    }
}

fn get_route(method: &str, path: &str, query: &str) -> Option<(u16, &'static str, String, bool)> {
    if method != "GET" {
        return None;
    }
    catalog_route(path, query).or_else(|| session_route(path, query))
}

fn catalog_route(path: &str, query: &str) -> Option<(u16, &'static str, String, bool)> {
    let body = match path {
        "/api/chat/workspaces" => workspaces_body(),
        "/api/skills" => skills_body(),
        "/api/sessions/search" => search_body(query),
        _ => return None,
    };
    Some((200, "OK", body, false))
}

fn session_route(path: &str, query: &str) -> Option<(u16, &'static str, String, bool)> {
    if path == "/api/sessions" {
        return Some((
            200,
            "OK",
            sessions_body(query_usize(query, "offset")),
            false,
        ));
    }
    let id = message_id(path)?;
    let body = messages_body(
        &id,
        query_usize(query, "offset"),
        query_usize(query, "limit"),
    );
    Some((200, "OK", body, false))
}

fn message_id(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/api/sessions/")?;
    let id = rest.strip_suffix("/messages")?;
    if id.is_empty() || id.contains('/') {
        None
    } else {
        Some(id.to_string())
    }
}

fn json_error(message: &str) -> String {
    json!({"error": message}).to_string()
}

fn query_usize(query: &str, key: &str) -> usize {
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == key {
            return value.parse().unwrap_or(0);
        }
    }
    0
}

fn response(status: u16, reason: &str, body: &str, cookie: bool) -> String {
    let cookie_line = if cookie {
        "Set-Cookie: hermes=preview; Path=/; HttpOnly\r\n"
    } else {
        ""
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cookie_line}\r\n{body}",
        body.len()
    )
}

fn workspaces_body() -> String {
    json!({
        "default_cwd": "/home/alice/.hermes",
        "home": "/home/alice",
        "repos": [
            {"root": "/home/alice/projects/hermes-native", "label": "hermes-native", "sessions": 4}
        ]
    })
    .to_string()
}

fn skills_body() -> String {
    json!([
        {"name": "b-plan", "description": "Plan the next change", "category": "workflow", "enabled": true, "usage": 3}
    ])
    .to_string()
}

fn search_body(query: &str) -> String {
    let q = query_text(query, "q");
    json!({
        "results": [{
            "id": "notes",
            "title": "Design notes",
            "source": "cli",
            "model": "demo",
            "role": "assistant",
            "snippet": format!(">>>{}<<< in the notes", q),
            "last_active": 1776000100.0,
            "message_count": 3,
            "pinned": false,
            "archived": false
        }]
    })
    .to_string()
}

fn query_text(query: &str, key: &str) -> String {
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == key {
            return value.replace('+', " ");
        }
    }
    String::new()
}

fn sessions_body(offset: usize) -> String {
    let rows = [
        session_row(NOTES, "Design notes", "cli", 3),
        session_row(BOARD, "Kanban board", "kanban", 1),
        session_row("older", "Older chat", "cli", 0),
    ];
    let total = rows.len();
    let page: Vec<_> = rows.into_iter().skip(offset).take(2).collect();
    json!({"sessions": page, "total": total}).to_string()
}

fn session_row(id: &str, title: &str, source: &str, count: usize) -> Value {
    json!({
        "id": id,
        "title": title,
        "source": source,
        "message_count": count,
        "last_active": 1
    })
}

fn messages_body(id: &str, offset: usize, limit: usize) -> String {
    let all = transcript(id);
    let page: Vec<_> = all.into_iter().skip(offset).take(limit).collect();
    json!({
        "messages": page,
        "pagination": {"order": "oldest", "returned": page.len()}
    })
    .to_string()
}

fn transcript(id: &str) -> Vec<Value> {
    match id {
        NOTES => notes_messages(),
        BOARD => vec![message("user", "Board item", None)],
        _ => Vec::new(),
    }
}

fn notes_messages() -> Vec<Value> {
    vec![
        message("user", "Summarize the layout.", None),
        message("tool", "", Some("web")),
        message("assistant", NOTES_MARKDOWN, None),
    ]
}

fn message(role: &str, text: &str, tool: Option<&str>) -> Value {
    json!({
        "message_uid": format!("{role}-{}", text.len()),
        "role": role,
        "text": text,
        "tool_name": tool,
    })
}

struct Hub {
    next: u64,
    approval: Option<String>,
    approval_runtime: Option<String>,
    models: HashMap<String, String>,
}

impl Hub {
    fn create(&mut self) -> (String, String) {
        self.next = self.next.saturating_add(1);
        (format!("stored-{}", self.next), format!("rt-{}", self.next))
    }

    fn model(&self, runtime: &str) -> &str {
        self.models.get(runtime).map_or("demo", String::as_str)
    }
}

async fn talk(mut socket: tokio_tungstenite::WebSocketStream<TcpStream>) {
    let _ = socket
        .send(Message::text(
            event("gateway.ready", "", json!({"heartbeat": false})).to_string(),
        ))
        .await;
    let mut hub = Hub {
        next: 0,
        approval: None,
        approval_runtime: None,
        models: HashMap::new(),
    };
    while let Some(frame) = socket.next().await {
        let Ok(frame) = frame else { break };
        match frame {
            Message::Text(text) => deliver(&mut socket, &mut hub, text.as_str()).await,
            Message::Ping(payload) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn deliver(
    socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    hub: &mut Hub,
    text: &str,
) {
    for frame in frames(hub, text) {
        if socket.send(Message::text(frame.to_string())).await.is_err() {
            return;
        }
    }
}

fn frames(hub: &mut Hub, text: &str) -> Vec<Value> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    if value.get("method").is_none() {
        return approval_result(hub, &value);
    }
    let method = value.get("method").and_then(Value::as_str).unwrap_or("");
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    method_frames(hub, method, &id, &params)
}

fn approval_result(hub: &mut Hub, value: &Value) -> Vec<Value> {
    let Some(runtime) = hub.approval_runtime.clone() else {
        return Vec::new();
    };
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    if hub.approval.as_deref() != Some(id) {
        return Vec::new();
    }
    hub.approval = None;
    hub.approval_runtime = None;
    vec![event(
        "message.complete",
        &runtime,
        json!({"status": "complete", "text": "Approved.\n\nThe preview tool did not run.", "usage": {"model": hub.model(&runtime)}}),
    )]
}

fn method_frames(hub: &mut Hub, method: &str, id: &Value, params: &Value) -> Vec<Value> {
    if let Some(frame) = session_frame(hub, method, id, params) {
        return frame;
    }
    if let Some(frame) = catalog_frame(method, id, params) {
        return frame;
    }
    if method == "prompt.submit" {
        return submit_frames(hub, id, params);
    }
    vec![ok(id)]
}

fn session_frame(hub: &mut Hub, method: &str, id: &Value, params: &Value) -> Option<Vec<Value>> {
    match method {
        "session.create" => Some(vec![create_result(hub, id, params)]),
        "session.resume" => Some(vec![resume_result(id, params)]),
        _ => None,
    }
}

fn catalog_frame(method: &str, id: &Value, _params: &Value) -> Option<Vec<Value>> {
    match method {
        "model.options" => Some(vec![ok_result(id, catalog_result())]),
        "config.set" => Some(vec![ok_result(
            id,
            json!({"scope": "session", "deferred": false}),
        )]),
        _ => None,
    }
}

fn create_result(hub: &mut Hub, id: &Value, params: &Value) -> Value {
    let (stored, runtime) = hub.create();
    let mut model = "demo";
    let mut provider = "custom:acme";
    if let (Some(requested_model), Some(requested_provider), Some(true)) = (
        params["model"].as_str(),
        params["provider"].as_str(),
        params["follow_profile_config"].as_bool(),
    ) {
        model = requested_model;
        provider = requested_provider;
        hub.models.insert(runtime.clone(), model.to_string());
    }
    ok_result(
        id,
        json!({
            "session_id": runtime,
            "stored_session_id": stored,
            "provider": provider,
            "model": model
        }),
    )
}

fn resume_result(id: &Value, params: &Value) -> Value {
    let stored = params
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if stored == BOARD {
        return err(id, 4132, "Kanban conversations are read-only.");
    }
    let (runtime, count) = if stored == NOTES {
        ("rt-notes", 3)
    } else {
        ("rt-other", 0)
    };
    ok_result(
        id,
        json!({
            "session_id": runtime,
            "message_count": count,
            "provider": "custom:acme",
            "model": "demo"
        }),
    )
}

fn submit_frames(hub: &mut Hub, id: &Value, params: &Value) -> Vec<Value> {
    let runtime = params
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = params.get("text").and_then(Value::as_str).unwrap_or("");
    let mut frames = vec![ok(id)];
    if text.to_ascii_lowercase().contains("approve") {
        frames.extend(approval_request(hub, &runtime));
    } else {
        frames.extend(markdown_reply(&runtime, hub.model(&runtime)));
    }
    frames
}

fn approval_request(hub: &mut Hub, runtime: &str) -> Vec<Value> {
    hub.approval = Some("approve-1".into());
    hub.approval_runtime = Some(runtime.to_string());
    vec![
        event(
            "message.delta",
            runtime,
            json!({"text": "Awaiting your decision.\n", "offset": 0, "segment": 0, "turn_id": "turn-approve"}),
        ),
        json!({
            "jsonrpc": "2.0",
            "id": "approve-1",
            "method": "approval",
            "params": {
                "session_id": runtime,
                "description": "Run the preview tool?",
                "command": "echo hello",
                "choices": ["once", "session", "always", "deny"]
            }
        }),
    ]
}

fn markdown_reply(runtime: &str, model: &str) -> Vec<Value> {
    vec![
        event(
            "message.delta",
            runtime,
            json!({"text": REPLY_MARKDOWN, "offset": 0, "segment": 0, "turn_id": "turn-reply"}),
        ),
        event(
            "message.complete",
            runtime,
            json!({"status": "complete", "text": REPLY_MARKDOWN, "usage": {"model": model}}),
        ),
    ]
}

fn ok(id: &Value) -> Value {
    ok_result(id, json!({}))
}

fn ok_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn event(kind: &str, session: &str, payload: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": {"type": kind, "session_id": session, "payload": payload}
    })
}

fn catalog_result() -> Value {
    json!({
        "provider": "custom:acme",
        "model": "demo",
        "providers": [{
            "slug": "acme",
            "name": "Acme",
            "is_user_defined": true,
            "models": ["demo", "other", "bad model"]
        }]
    })
}

const NOTES_MARKDOWN: &str = "\
## Saved note

A **bold** claim and a [safe link](https://example.com).

- alpha
- beta

> quoted line

`inline`
";

const REPLY_MARKDOWN: &str = "\
# Preview reply

This is **bold**, *italic*, and `code`.

- one
- two

```text
fn main() {}
```

| A | B |
| --- | --- |
| 1 | [docs](https://www.gpui.rs/) |
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_history_page_matches_the_limit() {
        let body = messages_body(NOTES, 0, 3);
        let value: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["messages"].as_array().expect("rows").len(), 3);
        assert_eq!(value["pagination"]["returned"], 3);
        assert_eq!(value["pagination"]["order"], "oldest");
    }

    #[test]
    fn first_session_page_reports_another_page() {
        let body = sessions_body(0);
        let value: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["sessions"].as_array().expect("rows").len(), 2);
        assert_eq!(value["total"], 3);
    }

    #[test]
    fn approve_prompt_asks_before_it_finishes() {
        let mut hub = Hub {
            next: 0,
            approval: None,
            approval_runtime: None,
            models: HashMap::new(),
        };
        let frames = frames(
            &mut hub,
            &json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "prompt.submit",
                "params": {"session_id": "rt-1", "text": "approve the preview tool"}
            })
            .to_string(),
        );
        assert!(frames.len() >= 3);
        assert_eq!(frames[2]["method"], "approval");
        assert!(hub.approval.is_some());
    }
}
