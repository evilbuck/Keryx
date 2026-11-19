---
date: 2026-11-19
updated: 2026-11-19
domains: [hermes, connection, transport]
topics: [reconnect, backoff, keepalive, websocket, timeouts, streaming, rpc, guardrails]
related:
  - .context/memory/hermes-connection-2026-11-08.md
  - docs/architecture.md
  - .guardrails/complexity-baseline.json
priority: high
status: active
artifacts:
  - src/backend.rs
  - examples/remote_smoke.rs
  - docs/architecture.md
  - docs/howto/run-hermes-native.md
  - changelog.md
---

# Connection hardening checkpoint (2026-11-19)

Nine-loop hardening pass over `src/backend.rs` transport. Branch `feat/loop-improvements`, commits `d6343fe`, `673081b`, `d3339ea`, `8961aa2`, `a1ccb8f`, `b981833`, `f788aab`, `7414196`, `424ef1e`.

## Durable decisions

1. **Automatic reconnect**: exponential backoff 1s, 2s, 4s, 8s, 16s, then 30s cap (`reconnect_delay`), reset on success (`reconnect_deadline`). Authentication rejections (login 401/404/429, ticket 401, scheme failure) set `Link.retryable = false` and are never auto-retried — a bad password must not hammer the login endpoint. Auth failure is the ONLY non-retryable class; network/ticket/HTTP failures stay retryable.
2. **A Reconnect press mid-sign-in aborts the in-flight attempt** (`open_link` loops on `OpenStop::Restart`); leaving the select block drops the pinned `connect()` future, which cancels its HTTP requests and socket dial. Dispatch via pure `mid_connect` (Queue/Restart/Shutdown).
3. **Idle keepalive is RFC 6455 Ping** after `PROBE_SILENCE` (30s) without any inbound frame; Pong refreshes `last_inbound`. A healthy idle connection is no longer dropped by the silence rule. `SILENCE_BOUND` (90s) is deliberately ≥ `COMPRESS_TIMEOUT` because a server-side compress may push no intermediate frames — do not tighten it to "speed up dead-link detection" without revisiting compress.
4. **Socket writes are bounded by `WRITE_TIMEOUT` (5s)**, far below the 30s RPC response wait: a gateway that stops reading fails the write in 5s and auto-reconnect takes over. tokio-tungstenite `connect_async` already sets TCP nodelay (verified in vendored source), so no Nagle workaround is needed.
5. **Streaming deltas never block the transport loop**: `emit_delta` uses `try_send` with a per-runtime pending tail capped at `DELTA_TAIL_CAP` (1 MB, UTF-8 boundary trim via `trim_to_cap`), retried each 5s maintenance tick; rollover drops the old tail (never concatenates out of order); `MessageComplete` supersedes parked tails (it carries authoritative text); `close_socket` clears them. The bounded event channel (`EVENT_BOUND` 512) stays bounded.
6. **On `gateway.ready`, queued commands drain BEFORE the studio catalog fetch**, and the two catalog HTTP requests run concurrently (`tokio::join!`); post-reconnect time-to-interactive does not wait on catalog RTTs. The UI's `note_connection` re-lists sessions and re-resumes the selected one on every Connected event, so auto-reconnect restores context end-to-end.
7. **Handshake decomposition** (`sign_in`, `request_ticket`, `ws_dial_url`, `dial_socket`), supervision decomposition (`fail_expired_jobs`, `heartbeat_ping`, `flush_pending_deltas`, `probe_keepalive`): backend.rs has zero baselined complexity hotspots; the guardrails complexity baseline is 4 entries and exactly matches the live CCN>10 warnings.

## Rejected with reasons (do not re-propose without new evidence)

- Cookie-reuse sign-in fast path (skip password-login when the session cookie is alive): no local evidence the gateway keeps the HTTP session across WS drops; wrong guess costs +1 RTT on every reconnect instead of saving one.
- Ping-timeout dead-link drop: an RPC error reply proves the link is alive; only total silence proves death, and the silence bound already covers it.
- Tightening `SILENCE_BOUND` below 90s: breaks the deliberate cover for a 90s server-side compress.
- Reqwest TCP `connect_timeout`: per-step sign-in bounds already exist via the 30s total request timeout.

## Verification snapshot

`cargo test` 113 passed; `cargo clippy --all-targets -- -D warnings` clean; coverage 56.8% (ratchet baseline 53.3%); guardrails lizard command: 4 warnings, all baseline-covered.
