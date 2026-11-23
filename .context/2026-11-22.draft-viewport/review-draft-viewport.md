---
status: completed
date: 2026-11-22
---
# Draft viewport bug-fix loop

User goal: edit and send long drafts at the minimum supported window size.

- Before: at 640×480, pasting 20 lines expanded the editor past the viewport and hid Send. Split text elements also painted the caret beside the first rendered line when its byte position was at the end of the draft.
- Functional defect: in `a\né`, Down from column one landed on byte 3, inside `é`, instead of byte 4. The native input and a focused Rust test both reproduced it.
- Fix: cap the draft area and let it scroll independently of the toolbar. Shape one text element, draw the caret at the measured byte position, and reveal it after navigation or editing. Vertical navigation now preserves character columns between logical lines without entering a UTF-8 code point.

## Verification

- Native 640×480 preview: with Flight Deck open and tools wrapped, the 20-line draft scrolled to the last line on insertion and back to the first line with Up. Selection stayed on the printed first line. The full Send button stayed visible; clicking it cleared the draft and added the turn to the transcript.
- Kept transcript spacing on the first and last cards rather than as a fixed minimum on the viewport. Find still scrolls to the first match at its previously verified 640×650 size. At 640×480 with Flight Deck and Find both open, the transcript remains visually cramped.
- The UTF-8 regression test failed on the baseline byte offset and passed after the change. Native Down landed after `é`, and Backspace removed the complete character.
- Review: no new input bindings or matching rules. Updated the architecture notes and saved Buck memory `d8489b6c-0f53-4ac3-838f-6236f5f8ccf3`.
- A 440-byte soft-wrapped line showed its tail after entry and its first words after Home.
- Final Buck guardrails passed unit tests, live remote smoke, lint, complexity, and coverage (62.3% against the 43.2% baseline). Patch coverage is disabled.
