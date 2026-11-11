---
date: 2026-11-10
domains: [desktop, rust, networking, testing]
topics: [model-picker, gateway, prime-switch, gtk]
subject: 2026-11-09.model-picker
artifacts: [implementation-model-picker.md, research-prime-contract-2026-11-09.md, plan-model-picker.md]
related: [src/model.rs, src/backend.rs, src/ui.rs, docs/architecture.md, examples/remote_smoke.rs]
priority: high
status: active
---
# Model picker

The composer has a provider/model picker. Catalog rows keep provider and model distinct and canonicalize user-defined providers to `custom:`. The switch value is unquoted `<model> --provider <provider> --session`. A prime on a never-run chat must be provider-less (`<model> --session`); an explicit provider makes the gateway acknowledge the switch without applying it.

An empty-chat acknowledgement is not proof. One live smoke run confirmed `deepseek-flash`; a later run's first inference reported `grok-4.7`. The client leaves a primed switch as "Switching to …" until `usage.model` arrives.

Busy mid-stream switches return `deferred: true` and apply on the next turn. That was proven on a disposable chat; the on-screen "next turn" label was not exercised. 640×480 layout was not resized. Orca computer-use works as `orca`, not `orca-ide`.
