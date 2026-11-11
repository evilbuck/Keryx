---
title: Add a searchable provider/model picker
status: active
priority: medium
created: 2026-11-09
updated: 2026-11-09
completed: null
related:
  - .context/2026-11-09.model-picker/plan-model-picker.md
  - .context/2026-11-09.model-picker/research-model-picker.md
---
# Add a searchable provider/model picker

Implement the model picker at the bottom of the native chat composer from the saved plan. Planning is complete; implementation has not started.

## Acceptance criteria
- [ ] Remote Hermes configured provider/model pairs appear in a native searchable popover; both provider and model remain visible.
- [ ] Quick fuzzy filtering and complete keyboard selection work without a new runtime or dependency.
- [ ] Session-only selection actually controls the next prompt, preserves chat isolation and reconnect/resume state, and never changes global defaults.
- [ ] Pending/failure/confirmation/unknown states, stale replies and unavailable/busy/read-only chats remain safe and truthful.
- [ ] Contract smoke, project checks and native Wayland acceptance pass; disposable fixtures are deleted and architecture docs are updated.

Start with the plan's disposable contract proof. `model.options` is live-verified; successful `config.set` switching is not. `session.info` is unsupported on the configured remote.
