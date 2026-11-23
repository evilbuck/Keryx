---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, transcript-find, responsive-height, keyboard-focus, approval]
related: []
priority: medium
status: active
subject: 2026-11-22.compact-find
artifacts: [review-compact-find.md]
---
A successful Find jump is not sufficient proof at 640×480; the result body must actually fit when Flight Deck is beside it. A focused short-window Find can temporarily omit the draft editor while retaining pending interaction and Stop. Route Find entry points through one focus path and close competing input surfaces, otherwise an observable Find focus can coexist with Commands still consuming keystrokes.