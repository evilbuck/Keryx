---
date: 2026-11-23
domains: [frontend, ui]
topics: [key-handling, find-bar, shortcuts, gpui, bugfix]
related: ["slash-menu-state-2026-11-23.md"]
priority: medium
status: completed
subject: 2026-11-23.ui-bug-loop-1
artifacts: ["src/ui/chrome.rs", "changelog.md"]
---

Loop 2 of the UI bug-hunt loop. Bug: `on_find_key`, `on_session_key`, and
`on_side_key` (src/ui/chrome.rs) appended `key_char` to their text fields
without checking modifiers, so Ctrl+V/C/X/A typed a literal letter into the
find query, session filter, or side-ask draft. `edit_search` (picker) and
`answer_edit` (clarify) already guarded; these three did not.

Fix: `fn is_chord(event) -> bool` guard (control/alt/platform) at the top of
the three handlers; chorded keys fall through unhandled instead of
corrupting field text. Chords still reach `global_chord` first, unchanged.

Verification: cargo test 179 pass (new `chord_tests::chorded_keys_are_ignored_by_text_fields`),
clippy -D warnings clean, lizard no new >CCN-10 in chrome.rs/ui.rs.
