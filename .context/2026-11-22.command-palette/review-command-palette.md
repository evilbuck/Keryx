---
status: completed
date: 2026-11-22
---
# Command palette bug-fix loop

User goal: fix a visible UI defect and the functional bugs found while exercising it, then close the work through Buck.

- UI defect: the command palette extended below the visible window at 1416×829; its Close control was offscreen. The shared sheet now fits the viewport and scrolls its rows independently of Close.
- Functional defect: Ctrl+K focused a GPUI handle not mounted by the palette or workspace chooser. Escape and typing did not reach the root handler. Focus is now registered on the shared sheet frame, and dismissal restores the composer draft and caret. Keyboard selection is limited to rendered rows and scrolls them into view.
- Native reproduction: launch `hermes-native --preview --agent-socket <private-dir>/control.sock`; send `text` to seed a draft, Ctrl+K, wait for sheet focus, then Escape. On the old binary the sheet stayed open and subsequent typing did not reach the composer.

## Verification

- Native Hyprland preview at 640×480: opened from Ctrl+K, scrolled to the last rendered command with Down, clicked Close, and typed into the unchanged draft. Reopening showed the first command again. Filtered to Workspace and activated it with Enter; typing reached the chooser, and Escape returned to the composer with the draft intact.
- Buck v2 guardrails passed unit, remote functional smoke, lint, coverage (62.4% vs. 43.2% baseline), and complexity. The final scroll-reset adjustment was then rebuilt and exercised by the native scenario above. Patch coverage is disabled by the contract.
- Acceptance and implementation standards review: no remaining issues in the sheet scope. Updated the architecture notes; there is no new shortcut.
- Buck memory: `d46bbabc-0993-482a-b0e4-a5d13adc5334`.
- Adjacent UI defect observed at 640×480: the header clips Reconnect. This is a separate layout surface, not part of the sheet fix.
