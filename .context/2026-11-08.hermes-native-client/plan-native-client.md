---
status: completed
date: 2026-11-08
updated: 2026-11-09
subject: 2026-11-08.hermes-native-client
spec: spec-hermes-native-client.md
research: [research-native-client.md, research-connection.md]
iterations: [iterate-native-client.md]
memory: [hermes-native-implementation-2026-11-09.md]
---
# Native client implementation

## User Goal
Use a lightweight native Hyprland application to create conversations and continue existing eligible chats on a remote Nous Research Hermes server.

## Settled decisions
- User selected existing Hermes sessions, not API-created-only conversations. That choice is not withdrawn.
- Rust + GTK4/libadwaita; native Markdown; no webview/local agent runtime.
- Target only the remote server configured by `HERMES_GATEWAY_URL`, `HERMES_USER`, and `HERMES_PASSWORD`. Do not use the local Hermes installation.
- Authenticate with `POST /auth/password-login` (`provider=basic`) and a `reqwest` cookie jar. Obtain a fresh one-use ticket from `POST /api/auth/ws-ticket` for each connection to `/api/ws?ticket=`.
- Use the remote server's observed JSON-RPC methods: `session.list`, `session.create`, `prompt.submit`, and `session.resume`. Keep runtime `session_id` separate from durable `stored_session_id` / `session_key`.
- Do not require `hermes-gateway-v1` or `runtime.describe`: neither was available in the live probe. Reuse generated protocol types only where a published schema matches the actual remote contract.
- Do not send or bundle `HERMES_BASIC_AUTH_SECRET` or the OpenAI API key. Do not deploy, start, or modify any Hermes server.
- No token persistence or secret logging. Remote transport must be HTTPS, or HTTP only inside an explicitly trusted encrypted tunnel (SSH local forward or Tailscale/WireGuard). HTTP is not TLS. Do not disable certificate verification. Do not accept arbitrary public plaintext HTTP.

## Implementation slices
All six slices are completed and verified on 2026-11-09. The original scope of each slice is retained below.
1. **Project and shared state:** generate the Rust binary project with Cargo; define connection, session, message, and UI state in `src/main.rs` and `src/model.rs`. Add GTK4/libadwaita and only the selected HTTP/WebSocket dependencies.
2. **Remote connection/session backend (`src/backend.rs`):** implement cookie login, ticket creation, one persistent JSON-RPC WebSocket, request correlation, session discovery/create/resume, message streaming, and truthful error/disconnect states. Run network work off the GTK thread. Never automatically replay an ambiguously accepted prompt.
3. **Native chat interface (`src/ui.rs`, `resources/style.css`):** implement the session sidebar, new-chat action, user-selected session resume, multiline composer, and conversation state. Preserve per-session histories/drafts and update only changed streamed messages. Surface server refusals without silently forking or replacing a conversation.
4. **Rich text (`src/markdown.rs`):** render Markdown as native selectable blocks, including headings, emphasis, lists, links, quotations, code, and tables. Escape markup, keep unsafe links inert, and provide copy actions. No embedded browser.
5. **End-to-end verification:** launch the real Wayland application against the configured remote gateway. Create isolated test chats, verify related turns and session switching, reconnect using durable IDs, and exercise failure states without modifying unrelated user chats. Measure release-process memory for idle, streaming, and long-history use.
6. **Project checks:** after source edits settle, resolve and run the Rust check contract once. Record actual results and update run/setup documentation after the working application is demonstrated. No build, test, or UI success is implied by this plan.

## Evidence and constraints
- The Cargo-generated Rust/GTK4/libadwaita client exists. There is no Electron, Tauri, webview, local agent, local Hermes target, installer, or second conversation database.
- Toolchain observed for the work: Rust 1.99.1, GTK 4.24.1, libadwaita 1.10.0.
- Historical remote probe (2026-11-08) remains in research-connection.md and verification.md: dashboard login, disposable two-turn context, reconnect/resume, and deletion of that test row. Existing non-test conversations were not prompted. `hermes-gateway-v1` was not negotiated and `runtime.describe` was unknown; those dated observations are not a permanent server contract.
- No Rust language server was configured; code-intelligence LSP status was queried before editing.
- Current implementation proof, including gates, native Wayland checks, RSS snapshots, and the conditional approval limit, lives in [verification-implementation.md](verification-implementation.md). Do not copy that numeric ledger here.
- Disposable fixture cleanup is [cleanup-verification.json](cleanup-verification.json): seven long-lived fixture ids deleted, HTTP history 404, absent from recent metadata. Failed earlier smoke ids were separately confirmed absent. No leftovers. An early keyboard-focus preview opened one unverified historical row with no prompt or delete.

## Acceptance evidence
Source implementation and acceptance are complete against the configured remote gateway only. Linked evidence, not this plan, holds the measurements:
- [verification-implementation.md](verification-implementation.md): final fmt/test/clippy, release build, real remote smoke, corrected interrupt probe, native Wayland proof, and RSS snapshots.
- [cleanup-verification.json](cleanup-verification.json): fixture deletion receipt.
- [iterate-native-client.md](iterate-native-client.md): integration corrections.
- [Session memory](../memory/hermes-native-implementation-2026-11-09.md).

Independent backend and native findings were corrected; narrow re-reviews returned no findings. A later one-line GVariant fix was proven by a throwaway real GLib roundtrip and an actual native four-question submission. Reviewers did not independently review that final line. The live approval card was not observed under server policy; shared string action serialization was proven.

## Closeout
There is no remaining implementation work in this plan. The verification ledger and session memory hold the exercised evidence; Git is the commit authority.

## Revision Log
### 2026-11-08 — Align implementation with the verified remote gateway
- Added: an explicit implementation starting point and ordered end-to-end verification.
- Modified: authentication, WebSocket lifecycle, protocol assumptions, proposed file ownership, and current evidence to match the successful remote probe.
- Removed: local-Hermes compatibility evidence and its use as a target (explicit user instruction). No product features removed.
- Inputs: session context, research-connection.md, verification.md, spec-hermes-native-client.md.

### 2026-11-09 — Implementation closeout
- Added: completed status, iteration and portable-memory links, and pointers to the verification ledger and cleanup receipt.
- Modified: evidence and acceptance from in-progress acceptance to verified closeout. Original six-slice scope and the user goal are unchanged.
- Removed: current claims that no application existed, that acceptance was still active, and that stop-ordering or remaining native/failure/RSS work was still open.
- Limits retained: approval card unobserved; Ctrl+C prose clipboard not claimed; final GVariant line not independently re-reviewed; no guardrails.json or coverage/complexity baseline; RSS snapshots are not peaks, PSS, GPU, or a fixed budget. One unverified historical row was previewed without prompt or delete.
- Inputs: parent acceptance facts, verification-implementation.md, cleanup-verification.json.
