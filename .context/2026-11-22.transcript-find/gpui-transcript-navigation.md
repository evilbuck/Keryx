---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, transcript-find, keyboard-focus, scroll]
related: []
priority: medium
status: active
subject: 2026-11-22.transcript-find
artifacts: [review-transcript-find.md]
---
GPUI ScrollHandle item indices refer to direct children of the tracked viewport, not nested message cards. Keep each message a direct tracked item when implementing search or outline jumps, stop following the bottom for explicit jumps, and verify actual rendered message visibility rather than only the match counter. Gate requests to focus conditional widgets on a successful open.