---
status: completed
date: 2026-11-22
---
# Compact header bug-fix loop

User goal: keep every header action usable at the supported minimum window size.

At 640×480, the native window showed only the left edge of Reconnect. The header now moves conversation tools to a second row below 760 logical pixels; connection status and Reconnect remain next to the title. On wider windows the header stays in one row. Long titles and branch captions truncate before clipping controls.

Baseline evidence: the previous loop's 640×480 compositor capture showed the header clipping Reconnect while the command palette was open. No connection behavior is changed by the layout fix.

## Verification

- Native Hyprland captures at 640×480 and 1000×600 showed every header control. The small window used two rows; the wide window used one.
- At 640×480, clicking Reconnect replaced the preview gateway TCP connection (`127.0.0.1:39112` → `127.0.0.1:54896`) and the window remained Connected. The connection label alone was too fast-moving to prove the click.
- Buck v2 guardrails passed on the final tree: unit tests, live remote smoke, clippy, coverage at 62.3% (baseline 43.2%), and complexity. Patch coverage is disabled.
- Acceptance and implementation standards review: no remaining issues in the header scope. Updated the architecture notes; no new user action or shortcut.
- Buck memory: `13436306-8a4a-43b5-8cce-b99d95a69fa9`.
