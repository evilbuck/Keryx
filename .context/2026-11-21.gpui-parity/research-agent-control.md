---
status: active
date: 2026-11-21
---
# Agent control of the GPUI window

The user requested an agent-controllable UI and screenshots as an alternative to desktop automation. GPUI 0.2.2 exposes `Window::dispatch_event` on real windows, so an opt-in local control interface can inject actual GPUI mouse and keyboard events rather than calling desk methods or changing state behind the UI. Source: https://docs.rs/gpui/0.2.2/gpui/struct.Window.html and the installed GPUI `src/window.rs`.

The interface must be explicitly enabled and limited to the local user: access implies authority to read private conversations and send prompts or approve operations. A private Unix-domain socket with owner-only permissions is preferable to an unauthenticated TCP listener; requests must be processed on the GPUI foreground thread, and replies should distinguish dispatch from verified UI effects. Screenshots need a real compositor capture path and must report failure or occlusion honestly; synthetic state snapshots are not screenshots. The existing `--preview` stub can provide disposable end-to-end verification without modifying real conversations.

The opt-in private socket supports state, keyboard input, mouse clicks, and screenshots. In the live preview, `hello\nworld` followed by Up moved the cursor from byte 11 to 5 without replacing the draft. A click at logical `(95, 292)` selected the disposable `notes` conversation and loaded three messages; hardened Hyprland toplevel capture via `grim -T` produced `/tmp/hermes-agent-38ZKlO/shot.png`, visually inspected. The pinned GPUI 0.2.2 snapshot is vendored under `vendor/gpui` with `DispatchEventResult` made public to expose its real event dispatcher. Fourteen transport tests passed. Final guardrails passed required gates; the advisory remote smoke failed because `session.compress` reported the session busy. Socket activation requires Linux 6.6 or later for `fchmodat2`.
