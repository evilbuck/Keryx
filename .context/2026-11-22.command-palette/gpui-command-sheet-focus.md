---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, keyboard-focus, command-palette, scroll]
related: []
priority: medium
status: active
subject: 2026-11-22.command-palette
artifacts: [review-command-palette.md]
---
GPUI key dispatch follows the mounted focus path. Creating and focusing a handle does not put it in that path: every branch sharing a handle must mount it, preferably on a common frame. Keep a short window's Close control outside a bounded scrolling body and bring keyboard-selected rows into view with its ScrollHandle. Use a local --preview agent socket to verify both key dispatch and compositor layout rather than trusting the observable focus label alone.