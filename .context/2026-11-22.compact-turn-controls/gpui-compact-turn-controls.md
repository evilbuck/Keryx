---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, responsive-height, approvals, stop, keyboard-focus]
related: []
priority: medium
status: active
subject: 2026-11-22.compact-turn-controls
artifacts: [review-compact-turn-controls.md]
---
At 640×480, pending approvals can make a full composer too tall, clipping the very controls needed to resolve the turn. Use the compact footer while awaiting user input, retain Stop outside the decision scroller, and give global keyboard shortcuts a mounted target until the editor returns. Verify real Deny and Stop clicks; the preview gateway only acknowledges interrupt, so Stopping proves dispatch, not a terminal event.