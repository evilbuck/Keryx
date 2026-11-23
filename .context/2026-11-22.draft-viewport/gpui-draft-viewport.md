---
date: 2026-11-22
domains: [ui, testing]
topics: [gpui, draft-editor, scrolling, utf8, caret]
related: []
priority: medium
status: active
subject: 2026-11-22.draft-viewport
artifacts: [review-draft-viewport.md]
---
For GPUI composer text, shape the draft as a single StyledText and map caret byte offsets through TextLayout. Use a bounded independent ScrollHandle and adjust the caret reveal after prepaint. Up/down map character columns back to valid byte boundaries. When testing at minimum size with the wrapping toolbar, check with the 320 px Flight Deck open as well: vertical transcript viewport padding can impose a minimum that squeezes out Send even when the draft viewport has a cap.