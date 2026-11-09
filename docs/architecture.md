# Architecture

Hermes Native is one GTK 4 / libadwaita process. The window is a sidebar plus a transcript plus a composer. Nothing here installs a package, a desktop shortcut, or a settings screen.

## Window

The sidebar lists server session metadata and offers **New conversation** (also Ctrl+N). Rows show the server title, source, and a read-only mark when resume is refused. Selecting a row does not create another chat.

The transcript is a `GtkListView`. GTK recycles the visible row widgets. Each row is native widgets from a per-block Markdown parse: headings, lists, links, code, and tables. Prose and code are selectable. **Copy original message** copies the stored text. **Copy code** copies that fence. Links open only after normalization, and only for `http`, `https`, and `mailto`. Other schemes stay inert. HTML is literal text. Images are alt text and are never fetched. There is no webview.

The composer is a multiline editor anchored at the bottom. Enter sends. Shift+Enter inserts a line. Stop is available only while a turn is busy. Clarify and approval controls appear only when the server asks. Choices are the server’s list, with allow-once and deny as the approval fallback when the server sends no choices. An approval card is implemented; whether a live server shows one depends on that server’s policy. This document does not claim a live approval card has been observed.

An interrupt acknowledgment means **Stopping**, not completion. Send remains blocked and Stop is disabled until the server's terminal event. Late text or submit acknowledgments cannot reopen a stopping turn. Late human requests and queued answers are cancelled rather than reopening controls or granting an action after Stop.

Drafts and the selected session stay in memory for the process lifetime, including across an explicit reconnect. They are not written to disk. Already loaded per-session histories stay in memory the same way. Quitting drops them. The next launch reloads from the server.

## Transport

`hermes-net` is a dedicated thread with a current-thread Tokio runtime. The UI thread only sends commands and drains events.

The dashboard gateway is used instead of the separate HTTP API server because the client must resume eligible existing CLI/TUI conversations, not only API-created chats.

Login is `POST /auth/password-login` with provider `basic` and a cookie jar. Each WebSocket open asks for a fresh single-use ticket (`POST /api/auth/ws-ticket`) and connects to `/api/ws?ticket=…`. TLS uses the default rustls verifier. Certificate checks are never disabled. Redirects stay on the same origin.

`stored_id` is the server’s durable conversation id. `runtime_id` identifies a live execution session; it is not the durable id and is not assumed to change with every socket. HTTP session metadata is paged newest-first in batches of 50. Saved message pages are internally chronological and use absolute oldest-first history offsets. The client keeps no authoritative local history database.

A refused resume (including RPC 4132, and a kanban source the client will not continue) loads the saved transcript as read-only. Refusal does not fork a new conversation.

Reconnect is the header button. It drops the socket and opens a new one. Prompts are not replayed. Explicit server rejection is a failure; an interrupted socket write or mid-turn disconnect leaves the outcome unknown. Partial text may be incomplete; the client does not guess that the server finished or failed. Empty or unrecognized terminal statuses are also unknown.

## Configuration

`main` loads `./.env` from the process working directory only. There is no parent-directory walk. Variables already set in the process are left as they are. Client configuration consumes only `HERMES_GATEWAY_URL`, `HERMES_USER`, and `HERMES_PASSWORD`. The URL must be an origin: no userinfo, path, query, or fragment. `HERMES_BASIC_AUTH_SECRET` is not consumed as client configuration, sent, or bundled.

`--trusted-tunnel` is the only behavioral flag. It allows `http` solely for loopback (`localhost`, IPv4 127.0.0.0/8, IPv6 loopback) and Tailscale (`*.ts.net`, 100.64.0.0/10, `fd7a:115c:a1e0::/48`). Other private addresses, including generic WireGuard ranges, stay rejected. Public HTTP is rejected. HTTPS does not need the flag.

No local Hermes process is required or contacted.

## Checks

Verification results, remote smoke details, and process RSS measurements: [implementation evidence](../.context/2026-11-08.hermes-native-client/verification-implementation.md). Run the project checks with:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Manual remote smoke, from the project directory, against the configured remote only:

```sh
cargo run --release --example remote_smoke -- --trusted-tunnel
```

The example always requires `--trusted-tunnel` as an explicit confirmation. It creates disposable chats, checks a two-turn context word against a second distinct chat, reconnects, and deletes what it created. It must not be pointed at conversations you want to keep. If cleanup fails it prints `disposable-id` lines; those ids are the operator’s to close. There is no coverage file and no `guardrails.json` in this repo.

Build and launch steps: [Run Hermes Native](howto/run-hermes-native.md).
