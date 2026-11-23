---
status: completed
date: 2026-11-22
---
# Compact composer bug-fix loop

User goal: keep writing and sending usable in the supported minimum window while retaining access to Conversations and Flight Deck.

- Before: at 640×480 with both rails open, 280 px went to Conversations and 320 px to Flight Deck. The remaining 40 px showed a letter-by-letter composer placeholder and no usable Send button. Even with only Flight Deck open, the single-line composer toolbar let Regenerate overflow the chat.
- Separate functional defect: with Conversations hidden, Commands → Filter & search all conversations changed focus to `session` without mounting the sidebar field. The command now opens Conversations before focusing search.
- Fix: use one rail when there would be less than 320 px for chat; a newly opened rail replaces the visible one. At wide sizes both rails still work together. Keep the model picker anchored to the wrapping composer toolbar. Send and Stop cannot be squeezed by the keyboard hint. If closing a rail removes the active text field and no other focus is pending, resume the composer.

## Verification

- Native Hyprland preview at 640×480: opening Flight Deck replaced Conversations; Regenerate and the entire Send button were inside the composer. The model picker opened over the wrapped tools, and Escape restored the draft. Clicking Send cleared the draft and grew the preview transcript from three messages to five.
- Commands → Filter & search all conversations reopened the hidden Conversations sidebar, focused its search field, and displayed the typed `design` query. Ctrl+I switched back to Flight Deck and restored composer focus. Both panels coexisted at 1000×600; resizing back to 640×480 removed the competing sidebar.
- Idle composer activity and interaction rows no longer reserve vertical gaps. Review found no remaining issue in the changed scope. Updated the architecture documentation and saved Buck memory `0d237087-f419-4a52-8ad5-c9bdbe55b7e6`.
- Final Buck guardrails passed unit tests, live remote smoke, lint, complexity, and coverage (62.2% vs. 43.2% baseline). Patch coverage is disabled.
