---
date: 2026-11-08
domains: [hermes, connection]
topics: [dashboard, auth, websocket, session-contract]
related:
  - .context/2026-11-08.hermes-native-client/research-connection.md
  - .context/2026-11-08.hermes-native-client/verification.md
priority: high
status: active
subject: 2026-11-08.hermes-native-client
---

# Hermes connection checkpoint (2026-11-08)

No application exists. Client credentials are `HERMES_GATEWAY_URL`, `HERMES_USER`, and `HERMES_PASSWORD` only. Do not send or bundle `HERMES_BASIC_AUTH_SECRET` or `HERMES_API_KEY`.

Remote dashboard password login succeeded (`POST /auth/password-login`, provider `basic`). WebSocket is `/api/ws?ticket=` after a fresh `POST /api/auth/ws-ticket` per connection. `hermes-gateway-v1` was not echoed. `runtime.describe` is unknown. Use `session.list`, `session.create`, `prompt.submit`, and `session.resume` as this server answered them. `reqwest` cookie jar plus one persistent JSON-RPC socket.

Smoke prompts, each sent once. The second does not contain alpha. Both `message.complete` replies were exactly `alpha`, so the second turn used the first prompt. Resume read `message_count` 4. Smoke row deleted. Existing-session writes were not tested.

The old API-key 401 is historical only.
