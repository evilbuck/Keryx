---
status: completed
date: 2026-11-22
---
# Compact turn controls

User goal: respond to approvals or stop a turn at 640×480 without opening Find or closing Flight Deck.

- Before: the native preview approval prompt was visible, but the normal composer pushed the approval choices and Stop below the viewport when Flight Deck was open. The hidden Stop could not be used.
- Build: share the compact Find footer with pending interactions in short windows, bound the decision form to its own scrolling region, and keep Stop outside it. Keep keyboard shortcuts on a mounted focus target while the editor is absent; restore editor focus when that target is no longer needed. No changes to turn or permission semantics.
- Native review: the 640×480 Hyprland preview showed all four approval choices and Stop with Flight Deck still open and Find closed. Find could open and close during the approval without editing a hidden draft. Clicking Deny completed the preview turn and typing went to the composer again. On a second pending approval, clicking Stop reached `Stopping response…`; the preview gateway only acknowledges the interrupt, so this is proof of dispatch, not server termination.
- Clarification shares the decision panel layout; only approval was exercised natively. Updated the architecture notes.
- Buck guardrails passed unit tests, live remote smoke, lint, complexity, and coverage (62.1% vs. 43.2% baseline). Patch coverage is disabled. Saved Buck memory `7b7117d5-259b-4a4e-b1e6-3e875d75a083`.
