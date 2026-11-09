---
status: active
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
topics: [hermes, dashboard, websocket, session-contract]
informs: [spec-hermes-native-client.md, plan-native-client.md]
---
# Connection architecture

## Recommendation

Talk to the remote dashboard named by `HERMES_GATEWAY_URL`, `HERMES_USER`, and `HERMES_PASSWORD`. Those three are the client credentials. `HERMES_BASIC_AUTH_SECRET` is a server-side signing secret. Do not bundle it or send it. Do not send `HERMES_API_KEY`. Dashboard password login is not HTTP Basic.

Use a `reqwest` cookie jar for `POST /auth/password-login` (`provider` `basic`) and later cookie-authenticated HTTP. Keep one persistent JSON-RPC WebSocket. On each connect, including reconnect, mint a fresh ticket with `POST /api/auth/ws-ticket` and open `/api/ws?ticket=`. Tickets are single-use. Do not replay a prompt after an ambiguous disconnect.

Call the methods this server answered: `session.list`, `session.create`, `prompt.submit`, `session.resume`, `session.close`, `session.delete`. Keep the runtime `session_id` distinct from the stored id (`stored_session_id` / `session_key`). Listing another source is not permission to prompt that row. Do not prompt or delete a session this client did not create.

This backend did not negotiate `hermes-gateway-v1` (the name was offered and not echoed). `runtime.describe` is unknown (`-32601`). Do not generate a client from a canonical schema this server did not advertise, and do not assume a newer main-branch contract.

No application exists. Implementation stays paused. Continuing an existing non-smoke session was not tested.

## Evidence

Remote only. Advertised Hermes 0.23.0 is a version string, not a commit. Origin is `http` on the same Tailscale host as the old API URL, port 9119, path `/`. Plaintext HTTP inside the encrypted tailnet. Not TLS, and not "TLS disabled." No certificate check was disabled.

| Observation | Result |
|---|---|
| `GET /api/status` | 200, `auth_required` true, providers include `basic`, uvicorn, no `WWW-Authenticate` |
| `GET /api/auth/providers` | interactive provider `basic`, `supports_password` true |
| `POST /auth/password-login` | 200; cookies `hermes_session_at`, `hermes_session_rt`, `hermes_session_provider` |
| `GET /api/auth/me` | `provider` `basic`; user id matches `HERMES_USER` |
| `POST /api/auth/ws-ticket` then `/api/ws?ticket=` | HTTP 101, then `gateway.ready` (`change_events`, `heartbeat`, `replay_epoch`) |
| `runtime.describe` | `-32601` unknown method |
| `session.list` limit 50 | metadata only; sources in that window: `cron`, `api_server`, `local`, `desktop`, `discord`. No user chat was prompted |
| `session.create` | runtime `session_id` distinct from `stored_session_id`; `lazy` true; profile `default` |

Disposable chat, each prompt sent once. The second prompt does not contain the word alpha, so an independent echo of the second instruction would not produce that word.

1. `Reply with exactly the single word alpha. Do not use tools.` `message.complete` status `complete`, text length 5, exact `alpha`. No tool events.
2. `What single word did I just ask you to reply with? Answer with that word only. Do not use tools.` Submit ack `status` `streaming`. `message.complete` status `complete`, text length 5, exact `alpha`. No tool events.

A second connection `session.resume` of the stored id returned `message_count` 4, and the transcript contained both `alpha` and the second question. After both sockets closed, another connection resumed the same `session_key` with those four messages. In that observation the runtime `session_id` did not change across disconnect. `session.close` then `session.delete` of the stored id succeeded. Exact-title lookup afterward returned no rows.

## Caveats

- Do not treat advertised 0.23.0 as a commit or as proof of a newer main-branch protocol.
- `hermes-gateway-v1` was not negotiated. `runtime.describe` is not available. Speak the methods above.
- Foreign-session `prompt.submit` was not tested. Do not adopt an existing user chat.
- The earlier API-key 401 is historical and is not this login. Detail stays in `research/notes-openai-api.md`. The local Hermes install is not the target.
- No secret values belong in logs or in the client binary.
