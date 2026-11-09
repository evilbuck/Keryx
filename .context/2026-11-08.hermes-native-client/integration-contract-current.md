---
status: completed
date: 2026-11-08
updated: 2026-11-09
completed: 2026-11-09
subject: 2026-11-08.hermes-native-client
---
# Current integration contract

`src/model.rs` is the produced shared contract. It supersedes the older proposed implementation-contract sketch. The behaviors below are the implemented contract, checked against the configured remote gateway and the parent-exercised native proof on 2026-11-09.

## History

- `SessionSummary.message_count` comes from the remote HTTP list; created chats have count 0.
- `Event::Resumed` carries `before`: the absolute stored-row index immediately before the loaded page, calculated as `message_count.saturating_sub(50)`. Use `order=oldest`, `offset=before`, `limit=message_count-before` for the initial page.
- `Command::History { stored_id, before }`: fetch `offset=before.saturating_sub(50)`, `limit=before-offset`, `order=oldest`.
- `Event::History { stored_id, before, next_before, messages }`: `before` echoes the requested cursor; `next_before` is the fetched starting offset. UI `Conversation::begin_history_page` prevents parallel duplicate pages; `receive_history_page` ignores stale or duplicate cursors.
- Display rows do not change this cursor. It is unaffected by optimistic or rejected prompts or later appended server messages. There is no arbitrary 200-row text truncation and no silent 200-row history cap. Only requested history is fetched. Native ListView recycling bounds visible row widgets while loaded data stays retained. Keep loaded histories and drafts per session in memory. There is no second database.
- Live remote probe confirmed `order=latest` selects a recent window but returns that window chronologically. It must not be reversed. `order=oldest&offset=1&limit=1` returned the second stored row. `before_id` and `before_row_id` query parameters were ignored and must not be used.
- `GET /api/sessions/<stored>` returned 404. Do not invent a detail route. A refused resume uses the HTTP list metadata count already cached during list, or discovers it through paged metadata listing if absent.
- Native proof loaded all 300 seeded rows. Earliest row boundaries were 251, 201, 151, 101, 51, then 1, after which the older control disappeared.

## Interaction

- `InteractionRequest` has `questions: Vec<ClarificationQuestion>` and `answers: HashMap<String, Option<String>>` in addition to approval prompt and choices.
- Questions carry `qid`, full prompt, string choices, and `multi_select`.
- `Command::Respond { runtime_id, request_id, answer: InteractionAnswer }`. Variants: `Approval(String)`, `Clarification(HashMap<String, Option<String>>)`, `Cancel`.
- Actual gateway `client.capabilities` advertised `canonical_clarify_batch=true`. The matching v0.23.0 OpenRPC question schema requires `qid` and `question`. Choices are string arrays or null. `multi_select` is boolean. Result `answers` maps qids to string or null. Cancel is a result without answers. Use separate per-question answers. Do not duplicate one answer across a batch. A null value is an explicit skip. Replayed answers are already locked and must not be edited.
- Multi-select is a JSON array encoded inside that answer string, not comma-separated text. The submitted native card recorded q0 `Blue`, q1 `[WebSocket,HTTP]`, q2 `live test`, and q3 skipped/null.
- The native clarify and approval actions are GTK `win` actions with parameter type `(ssv)`. `Variant::from` on the tuple inlines the third field and yields `(sss)`, `(ssb)`, or `(ssa{sms})`, which the typed action rejects. `Tuple::to_variant` boxes the third field as `v`. A throwaway GLib roundtrip proved all three shapes. That example was removed; the boxing remains in the action activation path.
- `Event::InteractionCancelled { runtime_id, request_id }` clears only the matching question through `withdraw_interaction`. It leaves an awaiting turn streaming, not failed.
- Preserve JSON-RPC server-request ID types when responding, number versus string. Do not add a legacy single-question shim.
- Approval controls must show the full server-redacted command or description, and only offered choices. Default safely to once or deny if choices are absent. Do not expose unoffered permanent or session permissions.
- A harmless approval-pattern command can succeed without the server emitting a card. Absence of a card is a server-policy limit, not permission to grant an unoffered choice. No live approval card was observed.

## Lifecycle

- Typed busy states are `Submitting`, `Interrupting`, and `Stopping`. An interrupt or stop acknowledgment is only an ACK. It is not terminal. The turn stays busy until the actual `message.complete` or gateway error for that generation.
- `Conversation::submit_acknowledged` accepts only `Submitting` or `Idle`. Idle covers a resumed running turn. It never reopens `Complete`, `Interrupted`, `Stopping`, or `AwaitingInput`.
- Backend tracks submit and interrupt generations and retires pending jobs when streaming, interaction, or terminal frames prove acceptance. Late acknowledgments and timeouts cannot affect a later turn.
- A rejected interrupt preserves busy state. A real gateway `error` notification finishes the backend turn and retires its jobs before the native error notice.
- A late approval or clarification request, or a queued reply, that arrives after Stop is not adopted and is not granted. The backend cancels it with the existing JSON-RPC error `-32000` cancelled. Native state refuses the late card. A late delta or ACK cannot reopen Stopping.
- `request.cancel` resolves only the matching interaction. The turn keeps running until its own terminal event.
- Only explicit `Command::Reconnect`. Never replay an ambiguous prompt. Initialization, handshake, writes, and close must fail visibly and be bounded. Shutdown can cancel connection work.
- Valid unrelated notifications, including `cron.changed`, are ignored. They are not chat failures. An unsupported server request still receives `-32601`.
- Only a policy or provenance refusal, such as observed resume code 4132, is read-only. Transient network failure, timeout, teardown, authentication failure, and unreachable host are not latched as a loaded read-only transcript. The unreachable profile is distinct from authentication failure. Neither creates a chat.
- Delete of an active session resumes, interrupts, closes, then deletes once. The retry flag survives close so a repeated active-session refusal cannot loop forever. An ineligible row is closed and deleted by stored id. Do not use resume as its cleanup fallback.
- No local Hermes target. Only the configured remote `.env`. No signing secret or API-server key is sent. No credentials, tickets, or cookies in output.

## Current verification

Final permanent tree, after the throwaway examples were removed:

- `cargo fmt --all -- --check` passed.
- `cargo test` passed 25 tests.
- `cargo clippy --all-targets -- -D warnings` passed.
- Release build passed after the final dispatch fix. `--help` and `--version` ran and reported 0.1.0.
- Real remote smoke and the corrected interrupt probe passed. Native Wayland proof covered the contract items above, including 300-row paging, submitted clarification, Stopping, and late cancellation.
- Same-process RSS for release pid 402166, and the completed fixture deletion receipt, are in `verification-implementation.md` and `cleanup-verification.json`.
- No durable guardrails, coverage, or complexity baseline exists. Do not invent one.
- A live approval card was not observed and is not claimed. Ctrl+C prose clipboard copy did not pass and is not claimed.
- The first compile failure, async constructor and GtkLabel wrap enum, was corrected before these gates. It is not the current status.
