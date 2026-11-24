---
date: 2026-11-23
domains: [frontend, ui]
topics: [gpui, composer, keyboard-focus, caret]
related: []
priority: medium
status: completed
subject: 2026-11-08.hermes-native-client
artifacts: [src/ui.rs, changelog.md]
---

GPUI `InteractiveElement::focus` refines an element's existing style; a focused border color won't paint without a border width. Track the composer handle on the input only, not also on the outer card. The empty draft previously returned before building a caret. The placeholder now shares the text layout/caret path; the existing UI timer flips the caret every 500 ms only for a focused editable input with no selection and restarts it on click or edit.

Verified with the native GPUI loopback preview and captured pixels: Find cleared the outline and caret, a click restored focus and the outline, the empty caret blinked on/off/on, and typing moved the visible caret to the end of the draft.