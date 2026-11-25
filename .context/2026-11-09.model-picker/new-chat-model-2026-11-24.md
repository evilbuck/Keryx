---
date: 2026-11-24
domains: [desktop, rust, networking, testing]
topics: [new-chat, model-picker, session-create]
related: [src/desk.rs, src/model.rs, src/backend.rs, docs/architecture.md]
priority: high
status: completed
subject: 2026-11-09.model-picker
---
# New-chat model selection

The fresh desktop preview reproduced the bug: connected blank composer, no selected session, clicking **Choose a model** left the picker closed. After explicitly creating a runtime, the same click opened it. The picker gate required a writable runtime even though the blank composer already supported writing a first message. That ruled out a missed hit target or an unavailable catalog. The separate historical pre-first-turn `config.set` ACK/inference mismatch ruled out creating a runtime ahead of the model choice as the reliable fix.

**New conversation** now opens the local composer. First send supplies the selected model/provider and `follow_profile_config: true` to `session.create`, then submits to that runtime. No override is sent without a pick. Failed creation keeps the draft and pick; successful creation consumes both. Already-created sessions retain their scoped switch workflow.

Native GPUI preview verification: clicked the startup picker; opened an existing transcript and clicked New conversation; selected `other`/Acme while preserving the draft and without creating a session; sent and observed a completed first reply with that pair; opened the next draft and confirmed the override did not leak. The loopback gateway reflects the create override in its reply usage. Prior live contract evidence is in `research-create-override-2026-11-09.md`.