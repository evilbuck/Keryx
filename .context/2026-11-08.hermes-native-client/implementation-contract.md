---
status: draft
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
---

# Implementation contract

**Status:** draft, 2026-11-08. No application source exists. Premature `Cargo.toml`, `src/main.rs`, and `src/model.rs` were removed. This file is a **proposed** canonical-WebSocket integration sketch. It is not authority to implement against the supplied API credentials, and it is not a claim that the deployed server speaks this contract. Paths below are proposed, not present.

Proposed shared types would live in `src/model.rs`. If that file is later created, Main would own it; agents must request any necessary API change rather than edit it. Main would own `Cargo.toml`, `main.rs`, `ui.rs`, and `resources/style.css`.

## Backend
Own `src/backend.rs`, `build.rs`, and `protocol/` (including canonical generation inputs/script). Expose `pub fn start(events: async_channel::Sender<BackendEvent>) -> async_channel::Sender<Command>`. Worker lifetime must end when command sender/event receiver closes. Keep all blocking/network work off GTK. Use a single current-thread Tokio runtime on a dedicated worker thread, with async multiplexing rather than blocking request/response loops that drop notifications. Runtime errors must reach BackendEvent::Error/Disconnected, not panic the UI.

Handle the commands in model.rs completely. On connect: allow HTTPS with normal certificate verification, or HTTP only when the peer is an explicitly trusted encrypted tunnel (SSH local forward or Tailscale/WireGuard). HTTP is not TLS. Do not disable certificate verification. Do not accept arbitrary public plaintext HTTP. Authenticate, then emit Connected and initial Sessions. The supplied credential is the HTTP API, not a dashboard token; do not target the dashboard backend with that key. Profile is a single selected connection scope, default `default`. Session-resume behavior of the deployed API is unverified until authentication succeeds.

Opened is authoritative selection/new-chat state with recent messages, opaque older cursor and actual writable/resumability decision. Failed resume should still present stored history read-only when allowed; never silently create a replacement. Session discovery/history may use HTTP or canonical RPC, but use actual supported contracts and preserve pagination.

PromptAccepted is emitted only after the server accepts the exact input_id, with original text. Main shows the accepted user message keyed by input_id. Streaming Message events must have stable per-message IDs; append=true means delta text, false means authoritative replacement. Avoid duplicate user echoes. Activity.busy=false is required on all terminal/error paths. Fetch authoritative display transcript after terminal completion if needed to reconcile live IDs. Never automatically resend an ambiguous prompt after disconnect.

ServerRequest carries ordinary upstream JSON-RPC requests; main displays approval/clarify/secret controls and replies using Command::Reply. Backend itself must reject unsupported request methods immediately with -32601 instead of waiting. Negotiate server_requests=true; honor request.cancel and replayed open_requests.

Generate Rust DTOs from a minimal reachable subset of upstream canonical OpenRPC/JSON schema at pinned revision ad4fa2f47cb689d4ca4a6ee465ab8d152b46cc73. Use a canonical JSON-schema generator (typify is available as a build dependency if suitable); keep no invented schema. Request DTOs must actually be used, not dead generated scaffolding. A script may derive the reachable subset mechanically from the pinned artifact. Main will add dependencies to Cargo.toml; report additions needed. Proposed available deps: anyhow, async-channel, futures-util, reqwest (rustls/json), tokio (rt/sync/time/net/macros), tokio-tungstenite (rustls), serde/serde_json, url, uuid, typify/serde_json/syn/prettyplease as build deps.

## Renderer
Own `src/markdown.rs`. Expose `pub fn render(text: &str) -> gtk::Widget` and optionally internal pure helpers. Only gtk (package gtk4), adw (libadwaita) and pulldown-cmark are guaranteed; use GtkTextView/labels for code without requiring GtkSourceView. No webview, images downloaded automatically, or arbitrary raw HTML interpretation.

Render paragraphs/headings/emphasis/strikethrough, ordered/unordered/task lists, blockquotes, links, inline/fenced code, thematic breaks and GFM tables. Every prose/code block is selectable. Code copy uses actual clipboard. Escape literal text and href markup. Only http/https/mailto links may launch externally; unsafe schemes remain inert. Show raw HTML as literal text and unsupported images as alt text, not a fake loaded image. CSS classes with prefix md- may be used; main supplies styles. Wrap prose; horizontal scrolling for wide code/tables; natural-height bubbles in a virtualized transcript. Strong care for parser stack nesting and partial Markdown during streaming. Whole-message Copy belongs to main UI.

## Verification
Every task must skip build/lint/tests/formatters mid-flight. Main runs once after integration; add focused permanent tests only for plausible consumer-visible boundary bugs (e.g. unsafe markup/link schemes or JSON event/ownership interpretation), not wiring/source text. No placeholder implementations, hardcoded example data, or success fallbacks.
