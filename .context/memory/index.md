---
date: 2026-11-08
updated: 2026-11-19
subject: 2026-11-08.hermes-native-client
status: active
---

# Memory index

- [connection-hardening-2026-11-19.md](connection-hardening-2026-11-19.md) — nine-loop transport hardening: auto-reconnect backoff with auth-failure exclusion, RFC 6455 keepalive, 5s write bound, non-blocking delta parking, ready-drain ordering; rejected alternatives with reasons; backend.rs and remote_smoke.rs have zero complexity hotspots.

- [transcript-role-styling-2026-11-11.md](transcript-role-styling-2026-11-11.md) — per-role transcript styling (user/assistant/tool/system) with tool chips; complexity refactor of steering hotspots; ratchet baselines updated (coverage 43.2, 12 hotspots).
- [model-picker-2026-11-10.md](model-picker-2026-11-10.md) — composer model picker; provider-less prime; empty-chat ACK is not proof after a grok-4.7 inference mismatch.
- [guardrails-init-2026-11-09.md](guardrails-init-2026-11-09.md) — durable guardrails.json v2 installed: baselines coverage 39.5%, 14 complexity hotspots, clippy clean; coverage wrapper .guardrails/coverage.sh.
- [hermes-native-implementation-2026-11-09.md](hermes-native-implementation-2026-11-09.md) — implemented Rust/GTK4 client; 25 tests and real remote/native acceptance passed; disposable fixtures removed; Stopping lifecycle and typed GVariant response lessons.
- [hermes-connection-2026-11-08.md](hermes-connection-2026-11-08.md) — remote dashboard login succeeded; cookie jar plus ticketed JSON-RPC WebSocket; old API-key 401 is historical.
