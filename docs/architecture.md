# Architecture

Hermes Native is one GTK 4 / libadwaita process. The window is a sidebar plus a transcript plus a composer. Nothing here installs a package, a desktop shortcut, or a settings screen.

## Window

The sidebar lists server session metadata, offers **New conversation** (`Ctrl+N`), and triages sessions locally into **All**, **Chats** (interactive sources such as `tui`, `local`, `api_server`, and messaging channels), **Auto** (automated `cron`, `heartbeat`, `webhook`, `kanban`, and background runs), and **Pinned** (`★`) with fuzzy local filtering plus live debounced server-wide FTS5 search (`Ctrl+F`). Pinned conversations float to the top of both the sidebar and Command Palette with a **`★ PIN`** badge. Rows show the server title, cleaned 1-line conversation preview, semantic source badge, message count, compact model badge, relative age, and a read-only mark when resume is refused. Hover never selects or resumes a row. Live `session.title` WebSocket broadcasts update the open header and sidebar row in place. Deleting the open conversation prompts for destructive confirmation (`AdwAlertDialog`) before sending `session.delete` and resets the window to the launch state on completion.

The header bar provides an **Omnisearch** (`Ctrl+K`) Command Palette (`AdwDialog`) for keyboard-first navigation on Hyprland: instant local fuzzy-jumping, live debounced full-server SQLite FTS5 search across thousands of remote conversations, and contextual actions (new chat, find in transcript `Ctrl+Shift+F`, pin/unpin `Ctrl+P`, rename `F2`, archive `Ctrl+Shift+E`, model picker `Ctrl+M`, Flight Deck `Ctrl+I`, sidebar toggle `Ctrl+B`, filter focus `Ctrl+F`, triage switches, history reload `Ctrl+R`, pagination, reconnect, and delete).

## Deep Recall (FTS5 Omnisearch & In-Transcript Find) and Session Curation

- **Server-wide FTS5 Omnisearch (`GET /api/sessions/search`)** — typing two or more characters in **Omnisearch** (`Ctrl+K`) or the sidebar filter (`Ctrl+F`) runs instant local matching and, after a 180 ms debounce, queries the gateway's SQLite FTS5 index across all unarchived server conversations (beyond the 50-row local sidebar page). Server `>>>match<<<` markers are strictly XML-escaped before rendering Pango `<b>match</b>` snippet highlights, with secrets redacted. Selecting any FTS5 hit opens or resumes the conversation and hands the query directly to the **In-Transcript Find Bar**, scrolling straight to the matching message.
- **In-Transcript Find Bar (`Ctrl+Shift+F`, header search button, or `/find [query]`)** — opens a native search bar above the transcript with a live `k / N` match counter, `Enter` (next) / `Shift+Enter` (previous) / `Esc` (close) navigation, `.message-match` / `.message-match-active` card highlights and badges in `GtkListView`, and a one-click **Search earlier pages** button when `history_before > 0`.
- **Server-persisted Session Curation (`PATCH /api/sessions/{session_id}`)** — conversations can be **Pinned / Unpinned** (`Ctrl+P`, header star button, `/pin`), **Renamed** (`F2` or header edit button opening a compact `AdwDialog`, or `/rename <title>`), and **Archived** (`Ctrl+Shift+E`, Command Palette, or `/archive`, hiding the session from active `archived=exclude` lists without permanently deleting server data). For newly created 0-turn sessions that have no SQLite row yet, pin/rename mutations apply optimistically and flush automatically once the first turn persists.

The **Flight Deck** (`Ctrl+I`, or the live context-budget badge on the right of the header bar) opens a collapsible inspector beside the transcript with three views:
- **Side-Ask (`/btw`)** — sends `prompt.btw` against a read-only snapshot of the active session (`Alt+Enter` or `/btw <question>` in the main composer, or directly inside the drawer) and renders `btw.complete` in Markdown with **Copy** and **↗ Insert into draft** promotion. Side questions work even while a turn streams or is stopping, never alter server history or turn alternation, and never fail or reopen an active turn if refused.
- **Live Trace** — displays session context-window occupancy, token/throughput/cost metrics (`session.usage`, `message.complete`, and `session.info`), the current turn's selectable reasoning stream (`reasoning.delta` / `thinking.delta`), and a live chronological tool timeline (`tool.start`, `tool.progress`, `tool.output_risk`, `tool.complete`).
- **Map (Outline)** — indexes user turns and assistant code-fence milestones so clicking any entry scrolls the virtualized `GtkListView` directly to that message, with a per-turn **`⑂`** action to fork a new conversation right after that turn.

## Non-linear branching, turn rewind, and lineage

Conversations are treated as navigable trees rather than append-only logs:
- **Point-in-time & whole-conversation branching** — any conversation with saved or streamed User/Assistant messages can be forked from the header bar (**Branch** / `Ctrl+D`), Command Palette (`Ctrl+K`), composer slash commands (`/branch [title]` or `/fork [title]`), Flight Deck Turn Map (`⑂`), per-message **`⑂ Branch`** pills, or the read-only banner's **`⑂ Fork into live chat`** button. Live sessions call `session.branch` (including server-wide `count` when forking mid-transcript across paginated histories); read-only or cold conversations call `session.branch_stored` and gracefully fall back to a seeded `session.create` (`parent_session_id` + visible transcript prefix) if the gateway refuses direct stored branching.
- **Live turn rewind (`session.undo`)** — idle writable sessions expose inline **`↺ Edit & retry`** on the latest user turn (rewinds the turn on the server and loads the prompt back into the composer for editing), **`↻ Regenerate`** on the latest assistant reply (rewinds and immediately re-submits for a fresh completion), and `/undo`, `/retry`, and `/regen` (`/regenerate`) slash commands plus Command Palette entries. After `session.undo` soft-archives the dropped SQLite rows, the backend refreshes the live session's authoritative `message_count` and tail history page before settling the UI.
- **Lineage navigation** — `GET /api/sessions` rows carry `parent_session_id`. Because the server collapses auto-compressed segments to a single tip row, Hermes Native marks **`⑂ FORK`** badges only on true forks (visible parent session, recorded branch, or `(branch)` suffix) and renders an interactive **Lineage Bar** above the transcript with one-click **`↖ Parent`** jump and a **`⑂ Branches · N`** popover menu for child forks.

The transcript is a `GtkListView`. GTK recycles the visible row widgets. Each conversation message uses native widgets from a per-block Markdown parse: headings, lists, links, code, and tables. Saved tool-result rows show the tool name when available, not their arguments or output. Prose and code are selectable. **Copy original message** copies the displayed message text (the summary for tool rows). **Copy code** copies that fence. Links open only after normalization, and only for `http`, `https`, and `mailto`. Other schemes stay inert. HTML is literal text. Images are alt text and are never fetched. There is no webview.

The composer is a multiline editor anchored at the bottom and focused on launch. Typing on the empty launch screen and pressing Enter creates a server-owned conversation titled from the opening prompt and dispatches the first turn once `session.create` confirms; if creation fails or disconnects, the launch draft stays intact and is never replayed. Inside an open conversation, Enter sends (or steers a running turn via `session.redirect`), Shift+Enter inserts a line, and Stop is available only while a turn is busy. Clarify and approval controls appear only when the server asks. Choices are the server’s list, with allow-once and deny as the approval fallback when the server sends no choices. An approval card is implemented; whether a live server shows one depends on that server’s policy. This document does not claim a live approval card has been observed.

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

A refused resume (including RPC 4132, and a kanban source the client will not continue) loads the saved transcript as read-only. Refusal never silently forks a conversation; instead, the read-only banner offers an explicit **⑂ Fork into live chat** action (`Ctrl+D`) if the user chooses to continue from that transcript in a new writable branch.

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

The example always requires `--trusted-tunnel` as an explicit confirmation. It creates disposable chats, checks a two-turn context word against a second distinct chat, runs an ephemeral `prompt.btw` snapshot side question and verifies saved history stays untouched, forks a point-in-time branch (`session.branch` with `count: 2`), proves the branch inherits context on a new turn, rewinds that turn via `session.undo` and verifies the server row count drops back, reconnects, pins and renames a chat via `PATCH /api/sessions/{id}`, verifies full-text `GET /api/sessions/search` returns Pango-highlighted FTS5 snippets, archives a disposable chat and confirms active list exclusion, switches one new chat to a second configured model, checks that the first inference reported that model, checks the server default did not move, and deletes what it created. It must not be pointed at conversations you want to keep. If cleanup fails it prints `disposable-id` lines; those ids are the operator’s to close.

The deterministic contract lives in `guardrails.json` (v2); run it with `npm run guardrails:check`.

Build and launch steps: [Run Hermes Native](howto/run-hermes-native.md).
