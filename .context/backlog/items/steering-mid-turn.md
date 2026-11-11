---
title: Implement mid-turn steering (session.redirect busy path)
status: active
priority: high
created: 2026-11-11
updated: 2026-11-11
completed: null
related:
  - .context/2026-11-11.steering/plan-steering.md
  - .context/2026-11-11.steering/research-gateway-steering.md
  - src/backend.rs
  - src/model.rs
  - src/ui.rs
---

# Implement mid-turn steering

Execute `.context/2026-11-11.steering/plan-steering.md`: enable the composer
during busy turns (except Stopping), dispatch busy Enter through
`session.redirect` with a queued `prompt.submit` fallback, handle
server-initiated FIFO-drain turn rollover, and verify with unit tests, a live
disposable-conversation probe, and the guardrails contract.
