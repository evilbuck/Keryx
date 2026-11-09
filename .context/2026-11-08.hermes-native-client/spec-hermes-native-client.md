---
status: completed
date: 2026-11-08
updated: 2026-11-09
subject: 2026-11-08.hermes-native-client
research: [research-native-client.md, research-connection.md]
plans: [plan-native-client.md]
iterations: [iterate-native-client.md]
memory: [hermes-native-implementation-2026-11-09.md]
---
# Hermes native client

## User Goal
A fast, low-memory native Hyprland desktop client for conversations with the user's remote Nous Research Hermes server, with multiple sessions and readable rich-text replies.

## Requirements
1. Native C or Rust implementation; no Electron, Tauri, browser engine, webview, or local agent runtime.
2. Persistent sidebar listing remote conversations, with a clear new-chat action and current-chat selection.
3. Create a real server-side chat, send a message, stream the assistant response, and continue the same conversation with its server-owned context.
4. Selecting a chat loads its existing transcript. Conversation identity survives switching and restarting the client; the server remains authoritative.
5. Render Markdown rich text: paragraphs, headings, emphasis, lists, quotations, links, inline/fenced code, and tables. Preserve original message text for copying. Raw HTML is not executable content.
6. Follow the supplied screenshot's broad sidebar/transcript/composer structure, not its product-specific navigation, tasks, voice, or integrations.
7. Use canonical project/protocol generators when available instead of manually duplicating generated code.

## Implementation
- **Language/UI:** Rust, GTK4, libadwaita; native Wayland. Cargo-generated client. No Electron, Tauri, browser engine, webview, local agent runtime, installer, or second conversation database.
- **Application layout:** responsive sidebar/content split. Restrained charcoal/dark presentation inspired by the reference, with system-theme-compatible native colors and accessible contrast. No second icon rail.
- **Sidebar:** recent conversation titles, selected state, scrollable list, new-chat control. Server errors are visible, not replaced by example data. Hover does not select history.
- **Conversation:** readable text column, clearly distinguished speakers, per-block selectable Markdown (prose, code, and table blocks). Whole-message and code copy actions retain source. Enter sends, Shift+Enter inserts a newline. Per-session drafts survive switch, read-only, and reconnect.
- **Identity:** server history is authoritative. Runtime session ids stay separate from durable stored ids. Eligible sessions resume; an ineligible session is refused read-only (observed RPC 4132) and is never silently forked or replaced.
- **Live behavior:** async I/O off the GTK thread; coalesce streaming updates to the active message. Background completion does not steal the selected chat. Do not force-scroll users who are reading older messages. Unknown broadcasts such as `cron.changed` are not chat failures.
- **History:** paginated fetching and recycled message widgets. Explicitly loaded history is retained; there is no silent 200-row cap. Client metadata is only what connection and preferences need.
- **Interaction safety:** show agent activity and clarification controls when the server requests them. ACK is interrupting/stopping and stays busy until the actual message complete or error. A rejected interrupt keeps busy state; a real gateway error retires the turn. Late requests after Stop are cancelled, never granted. A disconnect or interruption must not appear as successful completion and must not replay a possibly accepted prompt.
- **Approvals:** shared string action serialization is proven. A live approval card was not observed under server policy; that limit is conditional, not a claim that a card was shown.

## Connection contract
The system under test is the remote server in the updated project `.env`. A local Hermes install is not the target and is excluded from this contract.

`HERMES_GATEWAY_URL`, `HERMES_USER`, and `HERMES_PASSWORD` are the client credentials. `HERMES_BASIC_AUTH_SECRET` stays server-side and must not be sent or bundled. Do not send `HERMES_API_KEY`. Login is `POST /auth/password-login` with provider `basic`, not HTTP Basic. `GET /api/status` advertised Hermes 0.23.0 only. That string is not a commit.

HTTP uses a cookie jar (`hermes_session_at`, `hermes_session_rt`, `hermes_session_provider`). The socket is one persistent JSON-RPC WebSocket at `/api/ws?ticket=` after a fresh `POST /api/auth/ws-ticket` on every connect. This server sent `gateway.ready` and did not echo `hermes-gateway-v1`. `runtime.describe` is unknown. Use `session.list`, `session.create`, `prompt.submit`, and `session.resume`. Do not assume a newer main-branch schema.

Create returns a runtime `session_id` and a different stored id. Two related no-tools turns completed. The second prompt did not contain alpha; both completions were exactly `alpha`. A second connection and a later cold resume read `message_count` 4. The smoke row was deleted. Existing user chats were not prompted.

HTTP on this tailnet peer is plaintext inside the encrypted tailnet, not TLS and not "TLS disabled." Do not disable certificate verification. Do not accept arbitrary public plaintext HTTP. The previous API-key 401 is historical.

Existing non-test chats are not verification targets. Prompt or delete only tracked disposable conversations; never silently adopt another chat. The earlier implementation-contract.md sketch is not authoritative for this server.

## Native rendering constraints
- GTK does not supply a comprehensive Markdown widget. Parse Markdown and map it to native widgets; escape text before constructing Pango markup.
- Selection is per native prose/code/table block, not an automatically continuous cross-message selection. Exercised proof is PRIMARY selection and copy buttons. Ctrl+C prose clipboard was not proven. AT-SPI character-bounds and focus APIs are limited; that is not a product exception.
- Embedded arbitrary HTML, browser scripts, voice/calls, task cards, file attachments, a server-administration console, and an installer are not requested and are not part of this scope.
- Release-process RSS snapshots are recorded in [verification-implementation.md](verification-implementation.md). They are not peaks, PSS, GPU memory, or a fixed RAM budget. Do not duplicate that table here.

## Acceptance criteria for implementation
- Launch the actual native application under Wayland/Hyprland; verify sidebar, transcript, composer, and resizing visually.
- Authenticate to a real compatible Hermes backend. Authentication failure is distinguishable from an empty chat list and from network failure.
- Create a conversation, send two related turns, and verify server-maintained conversational context.
- Create a second conversation; switching between them preserves distinct histories and IDs. Relaunching retrieves the same conversations.
- Render representative headings, emphasis, list, quote, link, inline code, fenced code, and table correctly without a webview. Literal markup cannot become unintended UI markup.
- Slow network/tool execution does not freeze typing, sidebar navigation, or scrolling. Tool/approval/error terminal states do not leave a permanently running indicator.
- Test ownership/version limits: an ineligible historical session is read-only with an explanation; it is never silently replaced by a new session.
- Record release-process memory measurements for idle, streaming, and long-history use rather than promise an unmeasured RAM budget.

## Current status
Implemented and verified on 2026-11-09 against the configured remote gateway only. The local Hermes installation remains excluded. Gates, native proof, and RSS snapshots are in [verification-implementation.md](verification-implementation.md). [Fixture cleanup](cleanup-verification.json) confirmed seven long-lived ids removed, with no leftovers. See the [completed plan](plan-native-client.md), [corrections](iterate-native-client.md), and [session memory](../memory/hermes-native-implementation-2026-11-09.md). The live approval card remains unobserved under server policy.
