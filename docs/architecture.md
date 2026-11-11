# Architecture

Hermes Native is one GTK 4 / libadwaita process. The window is a sidebar plus a transcript plus a composer. Nothing here installs a package, a desktop shortcut, or a settings screen.

## Window

The sidebar lists server session metadata and offers **New conversation** (also Ctrl+N). Rows show the server title, source, and a read-only mark when resume is refused. Selecting a row does not create another chat.

The transcript is a `GtkListView`. GTK recycles the visible row widgets. Each conversation message uses native widgets from a per-block Markdown parse: headings, lists, links, code, and tables. Saved tool-result rows show the tool name when available, not their arguments or output. Prose and code are selectable. **Copy original message** copies the displayed message text (the summary for tool rows). **Copy code** copies that fence. Links open only after normalization, and only for `http`, `https`, and `mailto`. Other schemes stay inert. HTML is literal text. Images are alt text and are never fetched. There is no webview.

The composer is a multiline editor anchored at the bottom. Enter sends. Shift+Enter inserts a line. Stop is available only while a turn is busy. Clarify and approval controls appear only when the server asks. Choices are the server’s list, with allow-once and deny as the approval fallback when the server sends no choices. An approval card is implemented; whether a live server shows one depends on that server’s policy. This document does not claim a live approval card has been observed.

## Model picker

The composer's bottom-left trigger names the model and its provider, and opens a popover with a search field over the server's configured catalog. Rows keep provider and model together: the same model name under two providers is two rows, and a row whose name cannot be sent to the gateway (whitespace, or a leading `-`) is shown as unavailable instead of being silently rewritten. Search is local: it never sends a request and never selects anything. Hover does not commit; only an explicit row activation or Enter inside the search field does. Enter in the picker never submits the draft, and Escape returns focus to the editor.

The catalog is fetched once per connection with `explicit_only: true`, `include_unconfigured: false`, and `refresh: false`, and is dropped when the connection closes so a reconnect cannot show another connection's rows.

A switch is `config.set` with `session_id`, `key: "model"`, and the value `<model> --provider <provider> --session`. `--session` is mandatory: without it the gateway may persist the choice as the profile default, and a reply whose `scope` is not `session` is treated as a refusal. A value with quoted tokens is rejected by the gateway's `split()` tokenizer, which is why unrepresentable catalog rows are disabled rather than quoted.

A chat that has never run has no agent yet. The client sends a provider-less prime first, then the scoped switch. That acknowledgement is not treated as proof: a repeat against this gateway has had the first inference report a different model. The trigger stays on "Switching to …" until the completion's `usage.model` arrives, and a different reported model replaces the selection. A chat that has already run switches directly, and a session-scoped acknowledgement confirms that pair.

A switch made while a turn streams is answered `deferred: true`. It applies from the next turn: the running turn keeps the model it started with, and the trigger says so. A deferred reply is the only queued state the client shows.

The displayed model is never inferred from the catalog. It comes from create and cold-resume metadata, or from the completion's `usage.model` for an inference that actually ran. A pending switch reads "Switching to …", a deferred one reads "… · next turn", and a request that never answered reads "unconfirmed" and is not retried. A definite refusal or a cancelled confirmation keeps the previous selection and the draft. When the gateway returns `confirm_required` the client asks explicitly and only then repeats the same target with `confirm_expensive_model: true`.

Lazy `model.options` is not used as evidence of a chat's effective model: for an uninitialized session it reports the profile default.

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

The example always requires `--trusted-tunnel` as an explicit confirmation. It creates disposable chats, checks a two-turn context word against a second distinct chat, reconnects, switches one new chat to a second configured model, checks that the first inference reported that model, checks the server default did not move, and deletes what it created. It must not be pointed at conversations you want to keep. If cleanup fails it prints `disposable-id` lines; those ids are the operator’s to close.

The deterministic contract lives in `guardrails.json` (v2); run it with `npm run guardrails:check`.

Build and launch steps: [Run Hermes Native](howto/run-hermes-native.md).
