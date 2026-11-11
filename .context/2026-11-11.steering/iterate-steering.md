---
status: completed
date: 2026-11-11
updated: 2026-11-11
subject: 2026-11-11.steering
topics: [review, iteration, steering, turn-rollover, ui-sync]
informs: []
addresses: plan-steering.md
completed: 2026-11-11
from_review: b-review
---

# Iteration: steering

## Source
- Reviewed after: `/b-build`
- Plan: `plan-steering.md`
- Review: parallel standards axis (reviewer sub-agent) + acceptance-contract axis

## Critical Issues

### 1. Server-initiated turn rollover never reaches the UI
- **File**: `src/backend.rs:1389-1411` (rollover branch of `ensure_turn`), `src/ui.rs` (`apply`/`apply_stream`), `src/model.rs` (`receive_delta` is the only UI-side status restorer)
- **Problem**: When the gateway FIFO-runs a queued steer/prompt, the backend rolls the Terminal turn to Streaming silently — no event is emitted. The UI stays terminal (`Complete`/`Idle`) from `message.complete` until the first text delta arrives (`receive_delta` is the only path that sets `status = Streaming`). During that window (thinking/tool activity — the normal case after a queued steer):
  - Stop is hidden (`refresh_controls` shows Stop only when `busy()`), so the new live turn cannot be interrupted.
  - Enter dispatches plain `Command::Submit` (UI `steerable()` is false), and backend `begin_submit` overwrites the live rolled turn's tracking (`self.turns.insert` with a fresh submitting generation).
  - That plain busy `prompt.submit` hits the gateway's `interrupt` policy fallback: hard-interrupt of the live follow-up turn — exactly what this feature exists to avoid.
- **Proposed fix**: Emit a UI-visible transition when the backend rolls the turn (e.g. a new `Event::TurnRolled { runtime_id, message_id }`, or a typed model method that restores `Streaming` + activity without claiming `SubmitAcknowledged` semantics). `submit_acknowledged()` alone is insufficient — it only opens from `Submitting | Idle`. The UI handler must set status `Streaming` (not `Submitting`), set activity text, and keep the generation chain untouched, so `steerable()` becomes true and Enter steers instead of clobbering. Backend unit: rollover emits the event; model unit: handler restores Streaming from terminal without touching generations.

## Warnings

### 1. History pages now clone their message vector per page
- **File**: `src/ui.rs:1429-1430` (`apply_history` History branch, `messages.clone()`)
- **Problem**: The `apply` → `apply_history(&Event)` split forces borrowing the event, so every `History` page copies the whole `Vec<ChatMessage>` (plus every id/text `String`) on the GTK thread. Pre-split code moved it.
- **Suggested approach**: Route history events through an owning handler (`fn apply_history_owned(&self, state, event: Event) -> bool`) dispatched before the borrow-based fallback, or destructure `Event::History` in `apply` and pass fields by value. No behavior change intended.

## Recommended Workflow

Start with `/b-iterate` — it will pick up this file automatically.
Then re-run `/b-review` against the same plan.
Inside an OMP execution session, `/b-iterate` completes this artifact before it returns; the supervisor then re-reviews. Do not leave the file `active` waiting for review or save.

## Resolution (2026-11-11, /b-iterate)

- Critical 1 fixed: `ensure_turn` now returns `(&mut Turn, bool)` where the flag marks a server-initiated start (untracked turn with a server message id, or Terminal→Streaming rollover). `message.start` and `message.delta` handlers emit a new `Event::TurnRolled { runtime_id }` before the first delta. `Conversation::turn_rolled()` restores `Streaming` from settled states only (busy/Stopping guarded, never claims Submitting). Enter during the old blind window now steers; Stop is visible. Tests: `turn_rolled_restores_streaming_only_from_settled_states`, `delta_first_followup_rolls_an_untracked_turn_and_notifies`, extended `fifo_followup_rolls_terminal_turn_and_ignores_late_interrupt` with the notice assertion.
- Warning 1 fixed: `Event::History` is destructured and moved in `apply`; `apply_history` no longer clones page vectors (now handles only Deleted/Error/fallback).
- Verification: cargo fmt clean, clippy `-D warnings` clean, 43 tests pass (41 + 2 new). Release binary rebuilt at `target/release/hermes-native`.
