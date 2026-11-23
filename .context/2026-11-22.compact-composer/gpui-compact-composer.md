---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, compact-layout, side-panels, keyboard-focus]
related: []
priority: medium
status: active
subject: 2026-11-22.compact-composer
artifacts: [review-compact-composer.md]
---
A 640×480 Keryx window cannot display the 280 px Conversations sidebar and 320 px Flight Deck together without losing usable composer width. Resolve pane conflicts against the available chat budget, keep the model popover attached to the wrapping toolbar, and mount a target panel before focusing its text field. Verify both the rendered button bounds and a real click at minimum size.