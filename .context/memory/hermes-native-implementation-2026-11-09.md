---
date: 2026-11-09
domains: [desktop, rust, networking, security, testing]
topics: [hermes, gtk4, wayland, json-rpc, stopping, gvariant, native-markdown]
subject: 2026-11-08.hermes-native-client
artifacts: [plan-native-client.md, spec-hermes-native-client.md, iterate-native-client.md, verification-implementation.md, integration-contract-current.md, research-runtime-contract.md, cleanup-verification.json]
related: [README.md, docs/architecture.md, docs/howto/run-hermes-native.md, src/backend.rs, src/model.rs, src/ui.rs, src/markdown.rs]
priority: high
status: completed
---
# Native Hermes client implementation and acceptance

## Outcome
Implemented the full native-client plan with Cargo-generated Rust, GTK4/libadwaita, an off-main-thread dashboard JSON-RPC backend, and native Markdown. No browser engine, local agent, second authoritative history database, or local Hermes target was introduced.

The client authenticates with password-login/cookies and a fresh WebSocket ticket, lists and pages remote sessions, creates/resumes server-owned conversations, preserves per-session histories/drafts, and presents ineligible history as read-only rather than forking it. Streaming updates are coalesced; explicit reconnect never replays an ambiguously accepted prompt.

## Verified evidence
- Final permanent tree: 25 tests passed, strict Clippy passed, and formatting check passed. Release build, `--help`, and `--version` ran successfully. The Rust check contract is detected/ephemeral; no durable `guardrails.json`, coverage, or complexity baseline exists.
- Real Rust smoke completed two saved chats, two-turn context, switching, reconnect/resume, and cleanup. A separate real interruption→next-turn→reconnect probe passed after reproducing and fixing a late-completion race.
- Actual Wayland application verified native layout/resizing, Enter/Shift+Enter composition, distinct histories, cold resume, drafts, reload gating, responsive typing/navigation during a tool, and reconnect without duplicate prompt submission.
- Native rich headings/emphasis/lists/quotes/links/code/tables and literal unsafe markup were visually checked. Mouse prose selection produced PRIMARY `Literal`; whole-source and code-copy buttons produced exact clipboard text. No Ctrl+C prose clipboard success is claimed.
- Native history loaded all 300 fixture rows through earliest boundaries 251, 201, 151, 101, 51, 1. Exact native clarification submission reached the server as Blue, WebSocket+HTTP, `live test`, and a skipped/null fourth answer.
- Actual wrong-password window showed **Authentication failed**; the remote unreachable-port profile showed **Disconnected**. Send remained disabled with no permanently running indicator.
- Three same-process release RSS snapshots and cache/workload context are recorded in [the verification ledger](../2026-11-08.hermes-native-client/verification-implementation.md), not a promised RAM budget.
- All seven retained fixtures were deleted and confirmed absent by HTTP 404 plus metadata lookup. [Cleanup receipt](../2026-11-08.hermes-native-client/cleanup-verification.json). The smoke/probe chats also cleaned themselves up. Verification windows closed, input daemon stopped, listeners removed, original clipboard restored, temporary source probes/launcher removed.

## Corrections worth preserving
1. This gateway's `session.interrupt` result says interrupted but is an **acknowledgment**, not terminal completion. Keep the current generation and native **Stopping** state busy until actual `message.complete`/error. Late ACKs, deltas, interactions, and queued answers cannot reopen it or grant actions after Stop.
2. A typed GTK action expecting `(ssv)` needs tuple **`ToVariant`** conversion. `Variant::from(tuple)` inlines an existing Variant's concrete third type, producing `(sss)`, `(ssb)`, or `(ssa{sms})`; GTK rejects the action before the handler. All three fixed shapes round-tripped in a throwaway real-GLib probe, then the native batched answer succeeded end-to-end. No permanent wiring-echo test was added.
3. Saved pages are internally chronological. Absolute oldest-first offsets must not depend on optimistic display-row count. Preserve loaded data; recycle widgets instead of silently dropping text after 200 rows.
4. GTK single-click activation also selects on hover. Deliberate selected-notify, splice suppression, and complete pointer-intent retirement preserve selection/header/draft consistency and keyboard focus.
5. AT-SPI actions are asynchronous, recycled descendants may become defunct, and realized-row counts are not model counts. Wait for enabled controls and actual outcomes, guard the exact process and owned visible rows, and use real pointer/keyboard/clipboard evidence where the bridge lacks focus/character-bound APIs.

## Boundaries and limits
The configured remote server remained the sole authority. No server deployment/configuration or local Hermes execution occurred. Client configuration consumes URL/user/password only; signing secrets/API keys were not sent or bundled. `.env` was not modified or staged.

The server auto-approved a harmless shell-wrapper echo; a live approval card was not emitted. The approval path was source-reviewed and shares the proven string GVariant dispatch, but no live card observation is claimed. No dangerous command or policy change was used to force one.

Only tracked disposable chats were prompted/deleted. Early keyboard-focus troubleshooting previewed one unverified historical row without prompting/deleting it; do not claim that no historical row was ever opened.

## Workflow checkpoint
Independent backend/native findings were corrected and re-reviewed; the later one-line GVariant correction was verified by real serialization and native/server behavior. Setup/run/architecture docs and subject artifacts are consolidated separately.

Portable file-mode `/b-save` is used because `SQL_MEMORY_URL` is not configured; this is not an empty SQL-store result. The backlog contained only empty `items/` and `archive/` directories, with no linked item to archive or new deferred product work. This checkpoint precedes the requested local Conventional Commit on `feat/native-client`; no remote/upstream is configured.
