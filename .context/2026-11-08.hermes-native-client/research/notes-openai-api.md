---
status: complete
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
topics: [hermes, api-server, openai-compatible, owner-gating, live-probe]
informs: [notes-openai-api.md, sources-openai-api.md]
---
# Connection research — Hermes OpenAI-compatible API server (owner gating + live probe)

**Historical.** This file records the previous API-key configuration and its 401. It is not the verdict for the updated dashboard username/password login. The local Hermes install is excluded from compatibility decisions. Current authority is [research-connection.md](../research-connection.md).

Scope note: this file is owned by the runtime-verification worker. It records (a) source evidence
about the API-server owner gating and (b) live probe evidence against the user's deployed endpoint.
No application source was modified.

Evidence labels used throughout:
* **[SOURCE]** — read from upstream source (pinned commit or tag) or from the local install.
* **[LIVE]** — an HTTP response actually observed from the user's endpoint.
* **[INFERENCE]** — a conclusion drawn from the two above; not itself observed.

The owner-gate result in §3 is **[SOURCE]** evidence about *current upstream* behaviour. It was
**not** measured live: every authenticated live probe stopped at the 401 of §4, so no live
statement about what the deployed build refuses is claimed anywhere in this file.

## 1. Which Hermes surfaces exist (installed local upstream a51912c2 / v0.21.0)

Local install: `/home/buckleyrobinson/.hermes/hermes-agent`, `hermes --version` →
`Hermes Agent v0.21.0 (2026.8.31) · upstream a51912c2`.

Two HTTP surfaces exist and they are **different servers**:

| Surface | Where | Default port | Auth |
|---|---|---|---|
| API server (OpenAI-compatible) | `gateway/platforms/api_server.py` | 8642 | `Authorization: Bearer <API_SERVER_KEY>` |
| Dashboard (web UI + `hermes-gateway-v1` WS) | `hermes_cli/web_server*.py`, `tui_gateway/` | 9119 | dashboard session token / OAuth provider |

`gateway/platforms/api_server.py` module docstring (v0.23.0 upstream tag):
"Serves /v1/chat/completions, /v1/responses, /v1/models, /v1/capabilities, /api/sessions, …".

## 2. API-server route table (evidence)

Local install `gateway/platforms/api_server.py:78-101` (route tuples, name → (method, path)):

```
("health", ("GET", "/health")), ("health_detailed", ("GET", "/health/detailed")),
("models", ("GET", "/v1/models")), ("model_options", ("GET", "/api/model/options")),
("chat_completions", ("POST", "/v1/chat/completions")),
("responses", ("POST", "/v1/responses")), ("runs", ("POST", "/v1/runs")),
("run_status", ("GET", "/v1/runs/{run_id}")),
("run_events", ("GET", "/v1/runs/{run_id}/events")),
("run_approval", ("POST", "/v1/runs/{run_id}/approval")),
("run_steer", ("POST", "/v1/runs/{run_id}/steer")),
("run_stop", ("POST", "/v1/runs/{run_id}/stop")),
("skills", ("GET", "/v1/skills")), ("toolsets", ("GET", "/v1/toolsets")),
("sessions", ("GET", "/api/sessions")),
("session_create", ("POST", "/api/sessions")),
("session", ("GET", "/api/sessions/{session_id}")),
("session_update", ("PATCH", "/api/sessions/{session_id}")),
("session_delete", ("DELETE", "/api/sessions/{session_id}")),
("session_messages", ("GET", "/api/sessions/{session_id}/messages")),
("session_fork", ("POST", "/api/sessions/{session_id}/fork")),
("session_chat", ("POST", "/api/sessions/{session_id}/chat")),
("session_chat_stream", ("POST", "/api/sessions/{session_id}/chat/stream")),
("session_model_lock", ("POST", "/api/sessions/{session_id}/model")),
```

Upstream pinned `ad4fa2f47cb689d4ca4a6ee465ab8d152b46cc73` adds
`("run_unknown_resolution", ("POST", "/v1/runs/{run_id}/resolve-unknown", "run_unknown_resolution"))`
and `("capabilities", ("GET", "/v1/capabilities"))`; the v0.23.0 tag already has
`("GET", "/v1/capabilities", self._handle_capabilities)` (line 1560 of the v0.23.0 file).

### Session list / history / paging

* `GET /api/sessions` → `_handle_list_sessions` (local `:2738`, upstream `up_api_server.py:2520`).
  Query: `limit` (default 50, max 200), `offset` (max 1 000 000), `source`, `include_children`,
  `title` (exact-title filter; `include_hidden` honored only together with `title`).
  Response: `{"object":"list","data":[_session_response(s) …],"limit","offset","has_more"}`.
  `has_more` is computed from the *recency window* only (pinned back-fill does not count).
  `db.list_sessions_rich(..., order_by_last_active=True, include_pinned=True, include_archived=False)`
  → archived and hidden rows are excluded by default; sub-agent rows and compression
  continuations are hidden unless `include_children` (upstream `_session_filter_where`:
  `_LISTABLE_CHILD_SQL`, `_delegate_from_json(...) IS NULL`).
* `GET /api/sessions/{id}/messages` → `_handle_session_messages`. Query: `limit` (default page
  500, max 500), `offset`, `order` ∈ {`oldest`,`latest`}, `include_compacted`.
  Upstream resolves the lineage tip first: `resolved_id = db.resolve_resume_session_id(session_id)`
  and returns `{"object":"list","session_id":resolved_id,"data":[…],"pagination":{limit,offset,order,returned}}`.
  Local v0.21.0 has no `pagination` object — it returns `object/data` only.
* `POST /api/sessions` → `_handle_create_session`: body `id`/`session_id` (else
  `api_<epoch>_<uuid8>`), `title`, `system_prompt`, `model`, `model_config`, `source`.
  201 `{"object":"hermes.session","session":{…}}`; 409 `session_exists`; 400 `invalid_session_id`.
* `GET /api/sessions/{id}` → session row; `PATCH` → title/pinned/archived/unread/end_reason;
  `DELETE` → 200 `{"object":"hermes.session.deleted","id","deleted"}`, 404 `session_not_found`,
  409 `session_active_turn` (upstream, when a durable turn lease is held — `SessionActiveWriteGuardError`).
* `POST /api/sessions/{id}/chat` → one synchronous turn on that session
  (`hermes.session.chat.completion`, plus `X-Hermes-Session-Id` response header).
* `POST /api/sessions/{id}/chat/stream` → **SSE** with named events:
  `run.started`, `message.started`, `assistant.delta` (delta text),
  `tool.progress` (reasoning/tool preview), `tool.started`, `tool.completed`, `tool.failed`,
  `assistant.completed` (`{session_id,message_id,content,completed,partial,interrupted,runtime}`),
  `run.completed` (`{messages,usage,runtime,pending_steer?}`), `error`, `done`.
  Keepalive comment line `: keepalive` every `CHAT_COMPLETIONS_SSE_KEEPALIVE_SECONDS`
  (local 30.0 s; upstream main 10.0 s).

## 3. Owner gating — the central question

### 3.1 The gate is real and it is in the API-server binding path (upstream main/pinned)

`gateway/session_api.py` (pinned `ad4fa2f4`, fetched upstream) — `bind_api_steps()`:

```python
row = conn.execute('SELECT source,session_key,title FROM sessions WHERE id=?', (session_id,)).fetchone()
if row is not None and row['source'] != storage_source:      # storage_source == 'api_server'
    raise RuntimeStoreError('permission_denied')
```

and earlier, for a live session:

```python
if session_id in authority.sessions and not is_history_mount(authority.sessions[session_id]):
    if (authority.sessions[session_id].source.platform != Platform.API_SERVER
            or authority.db.get_session(session_id)['source'] != storage_source):
        raise RuntimeStoreError('permission_denied')
```

`restore_api_session()` re-checks the same invariants and raises `permission_denied` /
`admission_conflict` / `profile_mismatch` / `not_found`.

**Therefore: a session row whose persisted `source` is not `api_server` (i.e. a row created by
CLI, TUI, Desktop, messaging, `bot_room`, …) cannot be bound as an API-server conversation.**
This is the exact gate, and it is *not* a blanket statement — it is a per-row `source` comparison
in the API binding transaction.

### 3.2 What the API server still exposes for foreign sessions (read-only paths)

* `GET /api/sessions` lists rows regardless of source unless `?source=` is passed
  (`db.list_sessions_rich(source=source …)`; `source=None` ⇒ no source filter).
  → A native client can *see* CLI/TUI/Desktop sessions in the sidebar list.
* `GET /api/sessions/{id}/messages` only requires the row to exist
  (`_get_existing_session_or_404` → 404 `session_not_found`) and reads the transcript. It does
  **not** filter by source → history of a foreign-owned session is readable.
* `POST /api/sessions/{id}/chat` requires the row to exist, then runs a turn bound to that id.
  Whether it is refused depends on the binding path above: with a session authority active
  (upstream), a non-`api_server` row is refused (`permission_denied`); in the local v0.21.0
  install the same route exists and `_prepare_session_chat` does **not** visibly compare `source`
  — i.e. the refusal is enforced by the newer session-authority layer, not by the v0.21.0 route
  code alone. Do not claim refusal on v0.21.0 without an authority deployment.
* `POST /v1/chat/completions` with `X-Hermes-Session-Id: <id>` continues that session
  (upstream `api_server_openai_routes.py`): the header is 403-gated unless `API_SERVER_KEY` is
  configured ("Session continuation requires API key authentication"), the id is path-unsafe
  checked and length-capped (256), history is loaded from `state.db` via
  `db.get_messages_as_conversation`, and the id is advanced to the live compression tip with
  `db.resolve_resume_session_id`. There is **no source comparison on this path** in the route
  code; the authority layer is what fences cross-surface writes. A turn on a foreign row is
  therefore *possible in principle* but would take over a session another surface owns — the
  product must not do this.

### 3.3 Concurrency fences that also matter

* Durable per-session **turn lease** (`agent/turn_facade_lease.py`,
  `db.acquire_session_turn_lease`): a second process turning the same session *waits* and then
  continues, or fails with `session_turn_lease_timeout:<id>`. The lease is what keeps a native
  client from interleaving turns with a live CLI/TUI/Desktop owner.
* `db.delete_session(..., exclude_active_write_guards=True)` → 409 `session_active_turn` while a
  turn lease is held (upstream).

### 3.4 Documented API-server limitation (upstream docs, to be re-checked against the deployed version)

The API server's own session resource is the **API-server-owned** conversation set: sessions
created through `/api/sessions`, `/v1/chat/completions` (fingerprint-derived id), `/v1/responses`
(`conversation` / `previous_response_id` chaining), `/v1/runs`. Continuing a CLI/TUI/Desktop-owned
conversation is what the dashboard `hermes-gateway-v1` WS surface is for
(`session.resume`, `session.create`, `prompt.submit`, `session.interrupt`).

## 4. Live probe of the user's deployed endpoint (2026-11-08)

Source of truth: `/home/buckleyrobinson/projects/hermes-native/.env` (user-authored, 2 variables,
`HERMES_API_URL`, `HERMES_API_KEY`). Parsed as DATA by a script (never sourced/eval'd). No secret
value was printed, logged, or written to any artifact; only names, byte lengths and booleans.

Sanitized origin: `http://<tailnet-peer>:8642/v1` — scheme **http**, host is the user's own
Tailscale peer (`wooderson`, 100.78.88.21/32), port 8642 (= API-server default), path prefix
`/v1` (the OpenAI-compatible base). The peer is a direct Tailscale connection, so the bytes are
plaintext HTTP **inside an encrypted WireGuard tunnel**. TLS was not requested and verification was never disabled. Do not read that as "TLS disabled."

Observed live facts ([LIVE]):

| Probe | Result |
|---|---|
| `GET /health` (no auth) | **200** `{"status":"ok","platform":"hermes-agent","version":"0.23.0"}` |
| `GET /v1/models` | 401 `{"error":{"message":"Invalid gateway API key (API_SERVER_KEY)","type":"gateway_auth_error","code":"gateway_auth_failed"}}` |
| `GET /v1/capabilities` | 401 (same envelope) |
| `GET /v1/responses` (GET on a POST route) | **405 Method Not Allowed** → the route exists |
| `GET /v1/runs` (GET on a POST route) | **405 Method Not Allowed** → the route exists |
| `POST /v1/chat/completions` (empty body) | 401 (same envelope) |
| `POST /v1/responses`, `POST /v1/runs`, `POST /api/sessions` (empty body) | 401 (same envelope) |
| `GET /api/sessions?limit=1` | 401 (same envelope) |
| `GET /health/detailed`, `/api/status`, `/api/capabilities`, `/docs`, `/openapi.json` | 404 |

**Advertised version: `0.23.0`** — this is the `version` string the server *reports* in its
unauthenticated `/health` body. It is an advertised version, **not** a verified exact tag, build or
commit of the deployed code; no upstream tag/commit correspondence was established, and no
ordering claim relative to the local install or the pinned snapshot is made here. The only
transport fact observed is the response header `Server: Python/3.14 aiohttp/3.14.5`.

**Blocker (exact, alternatives not yet distinguished):** the credential in `.env` is rejected by
the endpoint with `gateway_auth_failed` / "Invalid gateway API key (API_SERVER_KEY)". The upstream
auth check compares the `Authorization: Bearer <token>` value byte-exactly against the server's
`API_SERVER_KEY` (`hmac.compare_digest(token.encode(), expected_key.encode())`, `api_server.py`
`_check_auth`, same shape in the local install, the `v0.23.0` tag and current main). **[SOURCE]**
The observed envelope is therefore what a *correctly formed* request with a *non-matching* key
produces; which of the following holds was **not** determined:

1. the endpoint's `API_SERVER_KEY` differs from the `.env` value (rotated key, or a value that
   belongs to a different instance/profile) — the "stale key" reading, plausible but **not
   established**;
2. the `.env` key is **profile-scoped** and the base URL is missing the profile prefix — upstream
   `_expected_api_key()` resolves the key per URL-selected profile (unprefixed request ⇒ launch /
   default profile key; `/p/<name>/…` ⇒ that profile's scoped secret), so a profile-scoped key
   presented on an unprefixed URL is compared against a different key;
3. the credential is not the API-server key at all (e.g. it belongs to a different Hermes surface).

Nothing above is asserted as the cause; each is an open alternative requiring the operator's
knowledge of the deployment. The client-side request construction was audited (§5) and is correct,
so the discrepancy is not attributable to the client.

Auth presentations tried against the same origin (all 401): `Authorization: Bearer <key>`,
`Authorization: <key>` (raw), `X-API-Key`, `X-Hermes-Api-Key`, `api-key`,
`Authorization: Bearer` + `X-API-Key` together. Because upstream accepts exactly one
presentation (`Authorization: Bearer <API_SERVER_KEY>`), every other shape is guaranteed 401
regardless of the credential's correctness; no further presentations will be tried.

No other host, port, or the dashboard surface (9119) was contacted. No credential store was read.
No session was created. Nothing was mutated.

### Consequences for the client design (still provisional)

1. On pin `ad4fa2f`, `bind_api_steps` fences API continuation to stored `source=api_server`. On local `a51912c2`, chat handlers can load a provided session id. Neither proves what this deploy does behind the 401. Do not treat sidebar visibility, history reads, or write refusal as measured live facts.
2. Dashboard `hermes-gateway-v1` and the HTTP API are different credentials. Advertised Hermes 0.23.0 does not make the dashboard the only cross-surface candidate, and it does not prove which session routes this deploy implements.
3. Route tables read from pin `ad4fa2f`, the v0.23.0 tag, and local v0.21.0 are source findings. They are not a live inventory of this server.

## 5. Local request-construction audit (offline only; no further network calls)

Done after the user asked for a parser-side audit before attributing the 401 to the server. No
request was sent during this audit.

**`.env` file hygiene** (`/home/buckleyrobinson/projects/hermes-native/.env`, 90 bytes, mtime
2026-11-08 19:42:21):

| Check | Result |
|---|---|
| Variable names present | `HERMES_API_KEY`, `HERMES_API_URL` (exactly 2 assignments, no duplicates, no other lines) |
| UTF-8 BOM | absent |
| CRLF line endings | absent (`\r` not present) |
| Quoting (`"` / `'`) around values | none present; values are bare |
| `export ` prefixes | none |
| `$` / `${...}` interpolation | none |
| Comment lines | none |
| Leading/trailing whitespace in values | none (0 leading, 0 trailing) |
| Value charset | ASCII, no control chars, no whitespace, no URL-unsafe chars (` ?#&/`) |
| `HERMES_API_KEY` length | 31 bytes |
| `HERMES_API_URL` | `http://<tailnet peer>:8642/v1` — scheme http, port 8642, path prefix `/v1` |

**Parser behaviour** (the probe's `load_env` helper; the throwaway script under
`/tmp/hermes_probe/` was removed after this audit was retained here): the file is opened and iterated
line-by-line as *data*; the file is never sourced, `eval`'d or `exec`'d. Blank lines and `#`
comments are skipped, each line is split on the **first** `=` only (so `=` inside a value would be
preserved), the key is stripped of surrounding whitespace and the value is stripped of surrounding
whitespace and one layer of matching quotes. Because this `.env` has no quotes/whitespace, the
parsed value is byte-identical to the file bytes.

**No environment override**: `load_env` reads *only* the file. The probe never consults
`os.environ` for either value, so an inherited `OPENAI_API_KEY`/`HERMES_API_KEY` cannot win.
Verified in the probe process: `HERMES_API_KEY` and `HERMES_API_URL` are **absent** from
`os.environ`; the only API-key-ish names present are unrelated (`BRAVE_API_KEY`, `CONTEXT7_API_KEY`,
`MULTI_MODEL_API_KEY`, `TYPESAFE_API_KEY`) and none is read by the probe.

**Header construction** (verified by reading the probe source and re-deriving the request offline):
the header is `Authorization: Bearer <parsed-key>` — one `Bearer` prefix, no double prefix (the
parsed key does not itself start with `bearer`), no whitespace inside the value, no trailing
characters. The request URL is derived by splitting the base URL and re-joining
`scheme://netloc` + the `/v1` path prefix + the route suffix, so the prefix is preserved and the
route is not appended to a wrong root. No query parameters are added, so no token can leak into a
URL, log line, or artifact.

**Proxy / redirect / TLS audit**: the probe process has **no** `*_proxy` environment variables and
`urllib.request.getproxies()` is empty — the request goes straight to the origin, so no proxy can
strip or rewrite the `Authorization` header. `urllib` follows redirects automatically, but the
origin answered the very first request (`GET /health` → 200) and every authenticated probe with a
direct 401 from the origin's own aiohttp server (`Server: Python/3.14 aiohttp/3.14.5`) — no
redirect chain was involved, and no cross-origin redirect could have forwarded the credential.
TLS: the origin is plaintext `http` on the user's own Tailscale peer, so the credentials travel
inside the encrypted WireGuard tunnel; TLS verification was never disabled and no certificate
validation was bypassed.

**Conclusion**: the request the probe sent was exactly `Authorization: Bearer <HERMES_API_KEY
from .env>` to `http://<peer>:8642/v1/<route>`. The parser is not at fault. The upstream check
(`hmac.compare_digest` of the token against the server's `API_SERVER_KEY`) is byte-exact, so a
correct request with a non-matching key yields precisely the observed
`gateway_auth_failed` envelope. Two server-side explanations remain as **open alternatives** (neither
is asserted), and both are **configuration-level, not client-level**:

1. the endpoint's `API_SERVER_KEY` differs from the value in `.env` (e.g. the key was rotated, or
   `.env` holds a key for a different profile/instance); or
2. the key belongs to a **named profile** served under a `/p/<profile>/` prefix. Upstream
   `_expected_api_key()` resolves the key *per URL-selected profile*: an unprefixed request uses
   the launch/default profile's `API_SERVER_KEY`, while `/p/<name>/...` uses that profile's
   scoped secret. The `.env` URL has no profile prefix, so a profile-scoped key would be compared
   against the wrong key and rejected. (The `v0.23.0` tag source also serves `GET /v1/health`
   alongside `GET /health`, and mirrors every native route under `/p/{profile}/…` — evidence that
   the profile-multiplexed listener exists in that tagged revision. Whether the deployed build
   contains it was not verified live.)

Both are resolved by the user/server operator, not by the client.

## 6. Open items / required user action

* Resolve the 401 on the server side (see §5): either the deployed `API_SERVER_KEY` differs from
  the `.env` value, or the key is scoped to a named profile and the base URL needs a
  `/p/<profile>/` prefix. Until one of these is settled, no API-server path can be exercised
  live, and the disposable two-turn continuity test cannot run. The client-side request
  construction has been audited and is correct (§5) — this is not a parser defect.
* Decide (with the user) whether the product target is:
  (a) API-server-owned conversations (only after auth succeeds and the deployed contract is measured), or
  (b) cross-surface resume of existing CLI/TUI/Desktop chats (a different credential; not established by the 401 or by the advertised 0.23.0 string).
