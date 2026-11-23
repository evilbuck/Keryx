---
date: 2026-11-23
domains: [frontend, ui]
topics: [slash-menu, composer, gpui, bugfix]
related: ["2026-11-22.compact-composer"]
priority: medium
status: completed
subject: 2026-11-23.ui-bug-loop-1
artifacts: ["src/ui/chrome.rs", "src/ui.rs", "changelog.md"]
---

Loop 1 of the UI bug-hunt loop. Bug: the composer's slash suggestion menu
kept stale selection state. Three symptoms, one root cause — selection state
(`slash_index`/`slash_hidden`) was reset only inside `insert_text`
(src/ui.rs), not on the shared edit path:

1. Backspace/delete kept the old selection index (typed chars reset it), so
   the highlight could point at a stale row after the token changed.
2. `insert_text` set `slash_hidden = false` unconditionally, so a menu the
   user dismissed with Escape reopened on the next keystroke.
3. Nothing clamped `slash_index` against the live candidate count, so a
   shrunk list (e.g. server skill refresh) killed both highlight and Enter.

Fix: pure transition `chrome::slash_state_after_edit(hidden, next_draft)`
called from `Root::replace` (shared by insert/backspace/delete/paste/cut);
selection resets to top on every edit, dismissal persists while the draft
still holds a slash token (`active_slash_prefix`), re-arms once the token is
gone; `chrome::slash_selection(index, count)` clamps in `slash_row`,
`slash_key`, and `accept_selected_slash`.

Verification: `cargo test` 178 pass (3 new unit tests in
`src/ui/chrome.rs`), `cargo clippy --all-targets -- -D warnings` clean,
lizard no new >CCN-10 functions, lcov line coverage 62.1% (baseline 43.2).
`npm run guardrails:check` script missing in this repo; runner script also
absent at skills/b-guardrails-check — gates run manually per guardrails.json.
Cargo.lock working-tree drift (windows-sys/getrandom swaps) predates this
session; excluded from the commit.
