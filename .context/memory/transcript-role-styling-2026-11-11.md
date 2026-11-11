---
date: 2026-11-11
domains:
  - frontend
topics:
  - transcript-styling
  - gtk-css
  - guardrails
related:
  - hermes-native-implementation-2026-11-09.md
priority: medium
status: active
subject: 2026-11-08.hermes-native-client
artifacts:
  - src/ui.rs
  - resources/style.css
  - guardrails.json
---

# Transcript role styling (2026-11-11)

Chat rows now differentiate roles visually. `role_style()` maps each `Role` to
row + speaker CSS classes; `message_body()` renders tool rows as compact
monospace chips (`system-run-symbolic` icon + "Used <tool>") and all other
roles through `markdown::render`. Styling in `resources/style.css`:
user = `alpha(@accent_bg_color, 0.12)` card + `@blue_2` YOU label
(`@accent_color` was too dark against its own tint), assistant = neutral
`alpha(@window_fg_color, 0.05)` card + `@success_color` HERMES label, tool =
orange label + chip, system = warning tint. Verified live on the real gateway:
history rows for all four roles screenshot-confirmed (ydotool click with
2x cursor-scale compensation, request = target/2).

Guardrails: complexity gate flagged pre-existing uncommitted steering-work
hotspots (`apply_stream` 11, `on_event` 16). Extracted `with_conversation()`,
`on_message_start()` (preserves ensure_turn → TurnRolled → retire order),
`on_tool_event()`; all now ≤ 10. Runner verdict: all gates pass; applied
ratchet updates — coverage baseline 42.6 → 43.2, complexity inventory
regenerated to 12 entries (on_event left). Lizard CSV parsing needs the csv
module: file = row[6], function = row[7], complexity = row[1].
