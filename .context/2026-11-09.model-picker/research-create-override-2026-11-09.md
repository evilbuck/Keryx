---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, session-create, remote-contract]
informs: [plan-model-picker.md]
---
# Model selection at session creation

A disposable remote probe verified gateway 0.23.0 `session.create` with `model: deepseek-flash`, `provider: custom:multi-model-gateway`, and `follow_profile_config: true`. The lazy create response and session catalog reported the requested pair, and the first completed inference reported `deepseek-flash`. The global model identity was unchanged. Both disposable sessions were deleted, and a complete session-list scan found neither fixture.

This differs from the existing client flow: it creates a runtime before the user can select a model. The earlier pre-first-turn `config.set` acknowledgement did not change inference. Between-turn switching, reconnect persistence, and isolation of an initialized switch remain unverified; the implementation gate remains open.
