---
status: active
date: 2026-11-21
---
# GPUI composer navigation parity

Verification: `cargo run -- --help` built successfully; the durable guardrails runner passed unit, functional, lint, coverage ratchet, and complexity gates. Neither command exercises this keyboard interaction.

The legacy GTK `TextView` handles up/down as multiline cursor navigation. GPUI previously called prompt recall before cursor movement for every unmodified up/down key, replacing multiline drafts when previous prompts existed. Multiline drafts now bypass prompt recall, preserving cursor navigation; single-line prompt recall remains available. Native interactive verification remains outstanding: the installed Orca computer-use guide command failed with `Unknown flag --require`, and the installed Paseo CLI exposes no `loop` command.
