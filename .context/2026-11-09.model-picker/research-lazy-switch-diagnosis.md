---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [gateway, lazy-initialization, model-switch]
informs: [plan-model-picker.md]
---
# Lazy-session model switching diagnosis

The configured remote returned a session-scoped success for `deepseek-flash --provider custom:multi-model-gateway --session` in a newly created lazy session, but the first inference and subsequent `model.options` both reported the original `glm-5.3`. This removes the earlier provider-token confounder: the same canonical token succeeded after an initial turn. Global settings remained unchanged.

The disposable sessions from this comparison were closed and deleted; an authenticated full session-list scan confirmed their absence.

A separate comparison timed out before producing a complete result. Its four disposable sessions were subsequently closed and deleted; a full 1,945-row listing found none of their fixture titles or IDs, and the global identity remained unchanged.

Upstream v0.23.0 source provides a candidate explanation: [`_set_model`](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_config_set.py#L157-L163) does not wait for a still-building agent when an explicit provider is supplied. [`_deferred_build_agent_kwargs`](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_registry.py#L98-L116) snapshots the model override during agent construction. A switch arriving after that snapshot but before the agent is attached could update the session override without updating the agent being constructed. This is a source-supported hypothesis, not a verified deployed-server trace; the running gateway reports version 0.23.0 but its exact revision is unknown.

Cold resume is independently misleading: the resume response and next inference showed the stored model, while pre-inference `model.options` reported the global default. Do not use lazy `model.options` as effective-state proof.
