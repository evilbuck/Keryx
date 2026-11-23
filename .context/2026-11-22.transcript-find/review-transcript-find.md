---
status: completed
date: 2026-11-22
---
# Transcript Find bug-fix loop

User goal: make Find useful in long transcripts and leave the draft usable afterward.

- Before: at 640×650, searching Design notes for `summarize` reported a match, but the viewport stayed on `Saved note` and hid `Summarize the layout.` Escape closed Find without restoring focus; keys stopped reaching the composer. The shortcut could also focus an unmounted Find field before any conversation was selected.
- Fix: transcript cards are direct scroll items. Find reveals the current match and moves the viewport on Next/Prev. It stops following the bottom during explicit navigation. The Flight Deck Jump action uses the same viewport operation. The Find button is disabled until there is a selected conversation, and Escape/Close return to the draft editor.
- Native verification at 640×650: the original transcript was reproduced with the preview gateway. Searching `a` showed the first match, Enter revealed `Saved note`, and Shift+Enter returned to `Summarize the layout.` Compositor captures and OCR verified actual message visibility. With no selected conversation, Ctrl+Shift+F left the composer usable. Both Escape and clicking Close preserved the draft and returned keyboard focus.
- A prefilled `/find summarize` revealed the same offscreen message. Flight Deck → Map → Jump returned from the later `Saved note` message to `Summarize the layout.` in the same native viewport.
- Buck memory: `7eb67662-eba0-442f-96cb-2dc4b69c7060`.
- Review: the native navigation changes only the scroll target and focus; Desk's existing find matching and wraparound stay unchanged. Updated the architecture documentation without introducing a new user action.
- Final Buck guardrails passed: unit tests, remote functional smoke, clippy, 62.2% coverage (43.2% baseline), and complexity. Patch coverage was disabled. An earlier smoke run received a gateway `session busy` refusal on compression; the final run passed without changing the smoke test or gateway behavior.
