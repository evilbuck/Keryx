---
status: completed
informs: []
---

# GPUI model picker placement and dismissal

In `hermes-native --preview --agent-socket PATH`, acknowledge the `sidebar` phase and wait for `transcript` (Design notes). At 1416×829 logical pixels, send `click` at `(449, 738)` on the model trigger, then at `(347, 27)` on Commands. Before the fix the socket reported `picker_open: true, sheet: "palette"` after the second click. The compositor capture showed the picker over the sidebar, to the left of its trigger.

`src/ui.rs` rendered the picker as a root overlay with fixed window offsets and no outside-press handler. The picker now lives in a deferred GPUI anchor under the trigger. Its capture-phase outside-press listener closes it without consuming the event or overriding the target control's later focus change. The trigger only installs an opener while the picker is closed, so clicking it again cannot undo dismissal. The panel blocks clicks through to covered controls.

The local GPUI preview passed search interaction, dismissal on a blank area, Commands opening and Conversations toggling on the same outside click, trigger re-click, reopening after sidebar reflow, and Escape returning focus to the composer. A compositor capture confirmed that the panel sits above the trigger. No live gateway was needed. Keep the separate model-switch backend acceptance item open.

The deterministic guardrails verdict was `pass`: unit tests, lint, coverage ratchet (62.6% vs. 43.2%), and complexity passed. The optional live-gateway smoke was pointed at a closed loopback endpoint to avoid remote mutations; it reported the expected connection failure.

## Narrow-window follow-up

At the minimum 640×480 window size, the first fix still shifted the 380-pixel picker left to fit the window. The picker background began at x=261; the conversation drawer ended at x=280. This reproduced the visual overlap on the anchored build. The pre-existing release executable was older still and did not recognize `--agent-socket`.

The anchor now uses the composer action row's measured bounds. It constrains the picker to that width and to the height above the row, then defers painting and hit-testing together. The picker still blocks clicks through its panel and dismisses on outside presses without consuming them.

Verified with the rebuilt `target/release/hermes-native --preview` at 640×480 on Hyprland. The picker began at x=308 and ended at x=612. Clicking its search field kept the picker open; clicking the `other` model closed it, registered picker index 1, and left Design notes selected. Reopening the picker and clicking Kanban board in the drawer intentionally selected that conversation and dismissed the picker.

Required guardrails passed (including lint and 62.5% coverage against the 43.2% baseline). The advisory gateway smoke was kept off the real gateway.
