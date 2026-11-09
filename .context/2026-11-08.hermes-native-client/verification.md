---
status: completed
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
owner: runtime-verification
scope: [updated-gateway-probe]
informs: [research-connection.md, spec-hermes-native-client.md]
---
# Verification — updated remote gateway

Connection probe against the updated project `.env` is complete. No application exists, so no GUI, build, or lint gate was run. That is out of scope here, not an unrun connection test.

## Current evidence

Remote dashboard password login succeeded. Not HTTP Basic. `HERMES_API_KEY` and `HERMES_BASIC_AUTH_SECRET` were not sent.

- `GET /api/status` → 200, `auth_required` true, provider `basic`, advertised Hermes 0.23.0, no `WWW-Authenticate`.
- `POST /auth/password-login` provider `basic` → 200. Cookies: `hermes_session_at`, `hermes_session_rt`, `hermes_session_provider`.
- `GET /api/auth/me` → provider `basic`, user matches `HERMES_USER`.
- `POST /api/auth/ws-ticket` then `/api/ws?ticket=` → HTTP 101 and `gateway.ready`. Offered `hermes-gateway-v1` was not echoed. `runtime.describe` → `-32601`.
- One disposable chat. Prompt 1: `Reply with exactly the single word alpha. Do not use tools.` Result: `message.complete` / `complete` / exact `alpha`. Prompt 2, which does not contain alpha: `What single word did I just ask you to reply with? Answer with that word only. Do not use tools.` Result: `message.complete` / `complete` / exact `alpha`. No tool events. Second connection and a later cold resume both read `message_count` 4. The smoke row was deleted. No user chat was modified.

Authority for the client shape is [research-connection.md](research-connection.md).

## Historical — previous API-key configuration only

Not the current verdict. The two-turn test **did** run later, against the updated dashboard login above. It had not run against the old API key.

The previous `HERMES_API_URL` probe saw `GET /health` 200 advertising Hermes 0.23.0, then 401 `gateway_auth_failed` on authenticated API routes. No chat was created in that probe. An offline parse audit did not find a quoting, BOM, Bearer, proxy, or inherited-env mistake. That did not prove the key was stale. Detail: `research/notes-openai-api.md`. Do not use a local Hermes install for compatibility decisions.
