---
date: 2026-11-18
domains: [frontend, backend]
topics: [tool-calls, skills, transcript]
related: [src/backend.rs, src/model.rs, src/ui.rs, docs/architecture.md]
priority: medium
status: active
---

# Tool activity context

Live tool notations retain the gateway context or argument preview, with configured credential redaction and a bounded single line. Completion preserves the initial context. Icons distinguish skills, search, terminal, editing, file reading and web activity. Saved tool history remains limited to tool names. Hermes TUI event fields are documented at https://github.com/NousResearch/hermes-agent/blob/main/ui-tui/README.md and friendly labels at https://github.com/NousResearch/hermes-agent/blob/main/agent/display.py. Durable guardrails passed including 107 tests, remote smoke, lint and complexity; coverage 56.5%. Native visual verification was unavailable.
