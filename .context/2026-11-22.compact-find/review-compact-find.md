---
status: completed
date: 2026-11-22
---
# Compact Find bug-fix loop

User goal: read transcript matches at 640×480 without closing Flight Deck or losing a draft.

- Before: Design notes reported a `summarize` match at 640×480 with Flight Deck open, but the message body was below the visible transcript strip.
- Functional bugs: Ctrl+Shift+F while Commands was open focused the Find field but left Commands routing typed keys to its own query. Submitting `/find summarize` opened Find but kept keyboard focus on the composer, so Enter did not navigate matches.
- Fix: below 560 logical px, a focused Find temporarily replaces the editor with transcript space. The draft stays intact; Escape or Close returns to it. Find activation switches from competing popovers, and `/find` hands off focus through the same UI path.
  - Native review caught a busy/approval edge: keeping the whole composer during approval pushed Stop below the viewport. Compact Find instead keeps the actual approval/clarification panel and Stop, without rendering the draft tools behind them.

## Verification

- Native preview at 640×480 with Flight Deck open showed the actual matched text, not just the match counter. Opening via Ctrl+Shift+F from Commands moved typed input into Find; Enter advanced to the next match. Escape restored the original draft and composer focus. `/find summarize` focused the same search field. At 640×650, Find and the composer remained visible together.
- At 640×480 with a real preview approval pending, the compact Find layout displayed all approval choices and Stop. Clicking Deny completed the fixture turn and removed the approval without dismissing Find. Clarification uses the same interaction renderer; the native scenario exercised approval only.
- Reviewed layout and focus transitions, updated the architecture notes, and saved Buck memory `b9feacab-5e0b-4d93-97ff-9d7570e79919`.
- Final Buck guardrails passed unit tests, live remote smoke, lint, complexity, and the coverage ratchet (62.2% vs. 43.2%). Patch coverage is disabled.
