---
date: 2026-11-18
domains: [frontend]
topics: [model-picker, new-session]
related: [src/ui.rs, docs/architecture.md]
priority: medium
status: active
---

# Launch model selection

Selecting a model on the welcome screen creates a blank conversation, preserving the launch draft and attachments, then requests the session-scoped model switch. First send waits for the switch response. The gateway may still report a different effective model on first inference, so the existing unconfirmed state remains authoritative. GtkListView single-click activation commits the activated position; hover only highlights. GTK reference: https://docs.gtk.org/gtk4/class.ListView.html.

Verification: 103 unit tests passed; live remote smoke, lint, complexity, and coverage guardrails passed (56.2% coverage). GTK desktop interaction was not visually verified.
