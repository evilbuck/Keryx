---
date: 2026-11-19
domains: [frontend, gtk-css]
topics: [command-autocomplete, contrast, accessibility]
related:
  - transcript-role-styling-2026-11-11.md
priority: medium
status: completed
subject: 2026-11-08.hermes-native-client
artifacts:
  - resources/style.css
---

# Command autocomplete contrast (2026-11-19)

The slash autocomplete rendered command tokens in `@accent_color`; under the active GTK theme this produced near-black text against the deck and selected-row backgrounds. The opaque-token color was changed to `@window_fg_color`, command/skill badges received more opaque fills and readable foregrounds, and descriptions no longer inherit reduced opacity.
The installed guardrails runner passed: required unit, ratchet, and complexity gates passed; advisory functional and lint gates passed; patch coverage was skipped because the gate is disabled. The documented `npm run guardrails:check` script is absent, and the runner is installed outside the repo.
