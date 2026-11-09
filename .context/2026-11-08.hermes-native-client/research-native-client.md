---
status: completed
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
topics: [hermes, rust, gtk4, wayland, remote-api]
informs: [brainstorm-hermes-native-client.md, spec-hermes-native-client.md]
---
# Native Hermes client research

## Conclusion
Use Rust with GTK4/libadwaita for a native Wayland client. For a client that browses and resumes existing Hermes conversations, integrate with the official remote dashboard backend using its HTTP session/history surface and canonical gateway WebSocket protocol. The HTTP API server is a valid alternative for API-owned conversations, but not an equivalent replacement for cross-surface session continuation.

These are researched recommendations, not an implementation claim. The deployed server version, endpoint, and session scope are not yet known.

**Later evidence (2026-11-08):** current transport, auth, and session decisions are in [research-connection.md](research-connection.md). The dashboard conclusion above predates the live probe and is not the authority for those decisions.

## Native stack
- Rust + `gtk4` + `libadwaita`: compiled native UI, no browser, webview, Electron, Tauri, or managed-language runtime.
- `pulldown-cmark`: parse Markdown into native selectable prose, links, code blocks, and tables. GTK does not provide a comprehensive Markdown renderer; this integration is real application work.
- GTK list-model/list-view recycling and paginated history: avoid constructing widgets for an entire unbounded conversation.
- Async Rust networking separated from the GTK main loop; choose WebSocket/HTTP dependencies for the selected Hermes interface rather than shipping both transports speculatively.
- Canonical setup: Cargo project generator plus GTK composite templates/resource generation. Existing local GTK 4.24.1/libadwaita 1.10.0 and Rust tools make this path feasible without installing GNOME Builder/Meson.

C+GTK has the same widget/rendering work with manual lifetime management. Iced offers built-in Markdown but its verified text-selection/accessibility tradeoffs are less suitable for this conversation reader. No benchmark establishes a specific memory advantage or RAM target for the unbuilt application.

Primary toolkit sources: [GTK Wayland](https://docs.gtk.org/gtk4/wayland), [gtk4-rs book](https://gtk-rs.org/gtk4-rs/stable/latest/book/), [NavigationSplitView](https://world.pages.gitlab.gnome.org/Rust/libadwaita-rs/stable/latest/docs/libadwaita/struct.NavigationSplitView.html), [pulldown-cmark](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/index.html).

## Remote Hermes interfaces
|Interface|Relevant behavior|Constraint|
|---|---|---|
|Dashboard backend|Official remote desktop backend; session/history HTTP routes; `/api/ws` canonical gateway methods for create/resume/prompt/interrupt and interactive controls|Requires remote `hermes dashboard`; authenticated connection, profile/ownership checks, compatible server version|
|API server|Official HTTP + SSE, API-key auth, `/api/sessions` list/create/messages/chat, stateful Responses and Runs APIs|API-owned sessions; cannot use OpenAI-compatible routes to continue CLI/TUI/Desktop/messaging-owned chats|
|ACP/local gateway attach|Structured local integration|Stdio or same-user local authentication; not a general network API|

Dashboard: default port 9119. API server: default port 8642, enabled by `API_SERVER_ENABLED=true`, started by `hermes gateway`.

Sources: [API server](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server), [dashboard remote backend](https://hermes-agent.nousresearch.com/docs/user-guide/features/web-dashboard#connecting-hermes-desktop-to-a-remote-backend), [gateway local sessions](https://hermes-agent.nousresearch.com/docs/developer-guide/gateway-local-sessions), [programmatic integration](https://hermes-agent.nousresearch.com/docs/developer-guide/programmatic-integration).

## Compatibility and safety invariants
- Sidebar visibility does not guarantee write/resume permission. Some legacy or foreign-owner transcripts are read-only; do not silently fork or replace them.
- Remote dashboard readiness alone is insufficient: authentication and live WebSocket negotiation must succeed.
- Use SSH-forwarded loopback or properly authenticated HTTPS/WSS with normal certificate verification. Treat agent access as authority to execute tools on the remote host.
- Interactive approvals/clarifications must be handled according to the negotiated protocol, not left spinning forever.
- Current main-branch documentation is not proof of deployed-version compatibility; inspect advertised capabilities and pin the generated contract to a tested upstream revision.
- Canonical gateway schemas are generated upstream in [gateway-contract.openrpc.json](https://github.com/NousResearch/hermes-agent/blob/main/apps/shared/src/gateway-contract.openrpc.json) by [gen_gateway_contracts.py](https://github.com/NousResearch/hermes-agent/blob/main/scripts/gen_gateway_contracts.py). Reuse them for type generation. No upstream REST OpenAPI document was found; do not fabricate one.

## Material product decision
Should the sidebar browse/resume the server's existing eligible Hermes chats, or only API-created conversations? This selects the initial backend contract and remote service setup. Recommend existing-session integration through the dashboard backend.

## Verification and limitations
Official documentation and source were inspected; supplied screenshot viewed; installed native toolchain/library versions queried. No application was built or launched, no remote server was accessed, and no memory benchmark was run. This session is documentation/research only; application test gates do not apply.

Detailed source capture and rolling findings: [notes](research/notes-native-client.md), [sources](research/sources-native-client.md).
