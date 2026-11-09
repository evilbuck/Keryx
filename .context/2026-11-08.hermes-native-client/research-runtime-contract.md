---
status: completed
date: 2026-11-08
updated: 2026-11-09
completed: 2026-11-09
subject: 2026-11-08.hermes-native-client
topics: [hermes, dashboard, websocket, runtime-contract]
informs: [plan-native-client.md, src/backend.rs]
---
# Runtime contract

Observed 2026-11-08 against the remote gateway in `.env` only. Advertised version string `0.23.0`. No local Hermes install was used as an execution, inspection, or compatibility target. No credentials, tickets, cookies, gateway address, or preexisting chat contents are recorded.

The remote server remains the authority. Upstream v0.23.0 source explains method names. It is not the contract where this server disagrees: empty WebSocket subprotocol, `runtime.describe` is `-32601`, `session.transcript` is `-32601`, and `session.list` rejects `offset` (`4000`, extra inputs not permitted). Those disagreements are dated observations, not a permanent guarantee that the remote server will never change.

Disposable probe rows used for this research, and the later long-lived verification fixtures, were deleted. The completed deletion receipt is `.context/2026-11-08.hermes-native-client/cleanup-verification.json` (`status: completed`, verified 2026-11-09T08:11:05.796Z). Exact-title lookup after the original probe returned no rows. Do not treat any listed fixture as still live.

## Login and socket

`POST /auth/password-login` JSON body:

```json
{"provider":"basic","username":"<user>","password":"<password>"}
```

`200` `{"ok":true,"next":"<path>"}`. Cookies, names only: `hermes_session_at`, `hermes_session_rt`, `hermes_session_provider`.

`POST /api/auth/ws-ticket` → `{"ticket":"<redacted>","ttl_seconds":30}`. Ticket length observed 43. One ticket per connect. Socket is `/api/ws?ticket=` with no subprotocol.

First frame:

```json
{"jsonrpc":"2.0","method":"event","params":{"type":"gateway.ready","payload":{"skin":{},"change_events":true,"heartbeat":true,"replay_epoch":"<string>"}}}
```

Later events add `params.session_id` and `params.seq`. After ready, `client.capabilities` `{"server_requests":true}` succeeds. Result methods: `approval`, `clarify`, `display.install.sudo`, `preview.act`, `preview.read`, `secret`, `setup_beat`, `sudo`, `terminal.read`, `theme.apply`, `theme.pick`, `tour`, `vault.code`, `vault.save_login`, `vault.unlock_prompt`, `window.read`. `declines_not_shown` and `canonical_clarify_batch` were true. This client answers only `approval` and `clarify`. Every other server request gets JSON-RPC `-32601` immediately.

## Sessions

List paging is HTTP, not RPC:

`GET /api/sessions?limit=&offset=&order=recent&archived=exclude`

```json
{"sessions":[{"id":"<stored>","title":"","source":"","started_at":0,"last_active":0,"message_count":0,"profile":"default"}],"total":0,"limit":0,"offset":0,"storage":{}}
```

`has_more` is `offset + returned < total`. Sidebar fields used: `id`, `title`, `source`, `last_active` (else `started_at`).

`session.create` `{"title":"<title>"}` →

```json
{"session_id":"<runtime-8>","stored_session_id":"<stored-22>","message_count":0,"messages":[],"info":{"lazy":true,"profile_name":"default"}}
```

Runtime and stored ids differ. Observed create source on the HTTP row is `tui`. Profile is `default`.

`session.resume` `{"session_id":"<stored>","inline_images":false,"omit_messages":true}` →

```json
{"session_id":"<runtime>","resumed":"<stored>","message_count":0,"messages":[],"messages_omitted":true,"info":{},"inflight":null,"running":false,"session_key":"<stored>","started_at":0,"status":"idle"}
```

A cold resume mints a new runtime id. Missing id is `4007` `session not found`.

Read-only is only a policy or provenance refusal, such as the observed 4132 Kanban resume refusal. Emit `Resumed` with `runtime_id: null`, `read_only_reason`, and the HTTP transcript. Do not replace the row. Transient network failure, timeout, teardown, authentication failure, and an unreachable host are not a permanent read-only latch and do not invent a replacement chat.

## History

Initial pages contain the most recent 50 HTTP rows, selected with an absolute oldest-first offset derived from the server's message count.

`GET /api/sessions/<stored>/messages?limit=<count-before>&offset=<before>&order=oldest&inline_images=false`

The later live probe confirmed that both oldest and latest windows are returned chronologically. Do not reverse them. `before = message_count.saturating_sub(50)` anchors the initial page. Older pages fetch from `before.saturating_sub(50)` up to, but excluding, `before`, so later appends and optimistic display rows cannot shift pagination. Empty and tool rows are preserved. A known zero count requires no history request.

```json
{"session_id":"<stored>","profile":"default","messages":[{"id":0,"role":"user","content":"","message_uid":"","tool_name":null}],"pagination":{"limit":50,"offset":0,"order":"latest","returned":1}}
```

`has_more` is `before > 0`, not a heuristic based on page length. HTTP metadata and successful resume expose message counts. WS `session.history` rows, if used, are `{role,text,timestamp,row_id}`. There is no silent 200-row cap. Native proof later loaded all 300 seeded rows of the since-deleted long-history fixture.

## Turns

`prompt.submit` `{"session_id":"<runtime>","text":"<text>"}` → `{"status":"streaming","user_row_id":0}`. A missing ack is not retried.

Events, same `message_id` (`payload.turn_id` from start/delta; complete has no `turn_id`):

```json
{"type":"message.start","payload":{"turn_id":"<id>"}}
{"type":"message.delta","payload":{"text":"","turn_id":"<id>","segment":0,"offset":0}}
{"type":"message.complete","payload":{"text":"","status":"complete","usage":{},"persisted_turn":{},"reasoning":""}}
```

`offset` is a UTF-16 cursor inside `segment`. Replay with a lower offset is dropped. `thinking.delta` and `reasoning.delta` are activity only; their text is not stored. Tool events use `name`, `tool`, or `tool_name` for activity text only.

`session.interrupt` `{"session_id":"<runtime>"}` → `{"status":"interrupted"}` even when idle. That result is an acknowledgment, not a terminal turn. The client stays in typed Interrupting or Stopping until the actual `message.complete` or gateway error for that generation. A late delta, late ACK, late interaction, or queued answer must not reopen the stopped turn. Late requests and queued answers are cancelled with existing JSON-RPC `-32000` cancelled and are never granted after Stop. Treating the ACK as terminal was a real probe failure on 2026-11-09; the corrected probe waited for the next turn.

A running resume (`running`, `status` of `streaming`, `running`, or `resuming`, or object `inflight`) is followed by `Submitted` so the UI leaves idle. No extra Event field.

## Delete and ignored noise

`session.close` `{"session_id":"<runtime>"}` → `{"closed":true,"messages":[]}`. `session.delete` `{"session_id":"<stored>"}` → `{"deleted":"<stored>"}`, or `4023` while active. Active delete resumes, interrupts, closes, then deletes once more. Ineligible rows are closed and deleted by stored id. Resume is not their cleanup fallback.

Known non-transcript events are ignored: `session.info`, `session.title`, `sessions.changed`, `platforms.changed`, `message.interim`. A valid unrelated notification, including `cron.changed`, is also ignored. It is not a chat failure. Unsupported server requests still receive their explicit protocol error. Do not surface every unrecognized event type as an error. No prompt is replayed. Reconnect is only `Command::Reconnect`.

## Verification scenario refinements

- On 2026-11-08 a disposable `source=kanban` row with two seed messages was visible via HTTP metadata, then `session.resume` refused with 4132 after runtime close. That was the safe live read-only fixture. It has since been deleted; see `cleanup-verification.json` id `20261108_220055_7201bb`, history 404. No preexisting user row was adopted. `source=api_server` alone was observed eligible; that fixture, `20261108_215025_f5ed1c`, is also deleted.
- The other long-lived fixtures in that receipt — the original 60-row fixture, native A, native B, native C, and the 300-row fixture — are likewise deleted and absent from recent metadata. Failed earlier smoke ids were confirmed absent separately. None of these ids are pending cleanup.
- Canonical batch clarification answers map qid to string or null. Multi-select is a JSON array encoded inside that string, not comma-separated text. See [upstream clarification tool](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tools/clarify_tool.py) and [TUI encoding](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/components/prompts.tsx).
- Benign tool proof can request `terminal` with `sleep 5; echo HERMES-UI-TEST-OK`, without files or network. A harmless `bash -c 'echo …'` can match an approval pattern and still complete without a live card. A card is policy-dependent. Never change server policy or run a dangerous command just to force one. The 2026-11-09 native run did not observe a live approval card. See [approval modes](https://hermes-agent.nousresearch.com/docs/user-guide/security#approval-modes).
- Kanban provenance refusal is source-backed in [resume guard](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/kanban_resume_guard.py) and was independently observed on the configured remote. Only that class of refusal is read-only. Transient network and timeout failures are not.
