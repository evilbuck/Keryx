---
status: active
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
topics: [hermes, native-ui, wayland, remote-api]
informs: [../brainstorm-hermes-native-client.md]
---
# Running research notes

## User request and reference
The client must connect to remote Nous Research Hermes, support separate chats/sessions in a sidebar, create chats, send/receive conversation messages, and render rich text. C or Rust only. Screenshot inspected directly: dark conversation list, transcript, bottom composer. Additional reference-product features are not automatically in scope.

## Local feasibility
- Empty application directory: only project AGENTS.md existed.
- Rust/Cargo, pkg-config, gtk4-builder-tool available.
- Installed native libraries: GTK 4.24.1, libadwaita 1.10.0 (`pkg-config --modversion gtk4 libadwaita-1`).
- No Git identity, prior .context memory, or backlog exists. Shared SQL recall was not queried without a reliable project identity.
- The b-research skill references docs/research-source-dictionary.md, but that file is missing; a filename search under the installed skills found no copy. Continue with official documentation and source code as primary evidence.

## Open research
Hermes protocol and native-toolkit investigations are complete. The product choice between existing-session integration and API-created-only chats remains unresolved.

## Official Hermes API-server documentation
Consulted the upstream Markdown source directly: https://raw.githubusercontent.com/nousresearch/hermes-agent/main/website/docs/user-guide/features/api-server.md (2026-11-08).

Verified: Hermes has an official OpenAI-compatible HTTP API server, enabled by `API_SERVER_ENABLED=true` and `API_SERVER_KEY`, started with `hermes gateway`, defaulting to `127.0.0.1:8642`. It streams SSE, including Hermes-specific tool-progress events and comment-line keepalives. The Responses API supports server-owned conversational state and chained replies. This contradicts an overly broad initial research-agent statement that there is no remote chat API; there is one. Protocol choice still needs the session-ownership restriction checked.

## Official remote-dashboard documentation
Consulted the upstream Markdown source directly: https://raw.githubusercontent.com/nousresearch/hermes-agent/main/website/docs/user-guide/features/web-dashboard.md (2026-11-08).

Verified: official Hermes Desktop can attach to a remote `hermes dashboard` process; the messaging gateway is a different service. Dashboard defaults to `127.0.0.1:9119`. Its readiness endpoint `/api/status` does not prove live chat works: chat uses separately authenticated `/api/ws` and `/api/pty` WebSockets and single-use tickets. Non-loopback binds require authentication and enforce host/peer guards. Username/password setup uses `HERMES_DASHBOARD_BASIC_AUTH_USERNAME`, `...PASSWORD`, and `...SECRET`. Existing sessions can be resumed through the dashboard; its browser Chat tab itself renders a remote TUI, which is not the proposed native client's rendering architecture.

Security implication: use SSH-forwarded loopback or correctly authenticated HTTPS/WSS rather than publicly exposing an unauthenticated agent-control surface. No remote server configuration has been changed.

## Session ownership and supported native integration
The Hermes researcher verified upstream source and developer docs:
- API-server sessions are distinct. The OpenAI-compatible routes refuse binding a CLI/TUI/Desktop/ACP/messaging-owned session with HTTP 409 `permission_denied`; current docs explicitly direct cross-surface work to gateway WebSocket `prompt.submit`.
- Dashboard `/api/ws` delegates to canonical `hermes-gateway-v1` dispatch with verified operator scope. `session.resume` can attach/adopt eligible same-profile sessions and `prompt.submit` can continue them; this is not merely transcript viewing.
- Resumability is not universal. Receiptless historical rows, foreign owners, other profiles, and server-version differences impose restrictions. Render unsupported rows read-only with an explanation; never silently create a replacement conversation.
- Generated canonical OpenRPC lives in `apps/shared/src/gateway-contract.openrpc.json`, produced by `scripts/gen_gateway_contracts.py`. Use this version-pinned upstream contract for generated native types; do not hand-invent a second protocol. No upstream OpenAPI document for the HTTP surfaces was found.
- Native clients must negotiate capabilities and correctly handle approvals/clarifications rather than presenting a permanently spinning conversation.
- The dashboard has a deployment session-token auth provider. Use supported header/ticket/WS credential flows over SSH-forwarded loopback or verified HTTPS/WSS; redact tokens from logs. Do not claim an unauthenticated `/api/status` response proves successful chat authentication.

Recommended product contract: dashboard backend for a genuine Hermes session client; API-server-only support is a distinct, narrower mode requiring explicit user agreement. The unresolved product question is whether the client must resume existing remote Hermes sessions or needs only API-created conversations.

## Native UI toolkit findings
Recommended stack: Rust, GTK4, and libadwaita. GTK provides native Wayland support, text selection/link activation, AT-SPI accessibility, and recyclable list widgets; libadwaita provides a responsive sidebar/content split. Rust avoids manual memory management without adding a managed-language runtime or webview.

No comprehensive upstream GTK Markdown widget was identified. Parse Markdown using `pulldown-cmark`, and render semantic native blocks: selectable labels for prose/links, read-only code widgets, and grids for tables. Escape markup-sensitive input. Render raw HTML literally, not as active HTML. Message blocks have independent selections; a single cross-conversation drag selection is not promised by this approach. Whole-message copy should retain the original Markdown.

Long histories should use paginated remote reads and a recyclable `GtkListView`; keep network work off the GTK main loop and update only the actively streaming message, not every message widget. No numerical memory claim has been benchmarked.

Compared alternatives: C+GTK has the same native widgets and Markdown integration work with more manual lifetime management; Iced has built-in Markdown but weaker verified text-selection/accessibility suitability for this use case. Neither changes the Rust/GTK recommendation.

Canonical generation: begin with official Cargo `cargo new`/`cargo init` as documented in the gtk4-rs Project Setup guide; use GTK composite templates and standard resource compilation. Do not introduce a Meson/Builder installation just to scaffold this project. Generate protocol types from pinned upstream schemas where supported; do not create an invented REST OpenAPI contract.
