# Steering build session memory

date: 2026-11-11
subject: 2026-11-11.steering
topics: [steering, session.redirect, busy-turn, turn-rollover, complexity-gate]
status: active

## What was implemented (b-build, standard)

- `src/model.rs`: `TurnStatus::steerable()` (Submitting|Streaming|AwaitingInput,
  Stopping closed), `SteerOutcome {Redirected, Queued}`, `Command::Steer`,
  `Event::SteerAccepted`, `Conversation::steer_acknowledged` — never reopens
  Stopping, never claims Submitting; redirected upgrades Submitting→Streaming.
- `src/backend.rs`: `Command::Steer` routed by `steer_route(Option<&Turn>)` →
  Redirect (live phase) / Submit (idle-or-terminal race safety net) / Drop
  (Stopping). `JobKind::Steer` accepts `redirected|queued`; `rejected` sends
  queue-only `prompt.submit {session_id, text, queued: true}`
  (`queued_submit_params`, `JobKind::Queue`). Steer/Queue errors surface as
  `Event::Error {operation: "steer"}` — deliberately not "submit", so
  `operation_failed` never ends the live turn. `ensure_turn` now ROLLS a
  Terminal turn when a new non-empty message id arrives (FIFO-drained
  follow-up): new generation, fresh segment/offset, prior turn jobs retired;
  late interrupts of the old generation are ignored.
- `src/ui.rs`: Send enabled when writable && !reload_pending && draft non-empty
  && status != Stopping (busy no longer blocks). `submit()` routes busy
  (steerable) input to new `steer()`: optimistic row, draft clear,
  `Command::Steer`. `apply` split into `apply_history`/`apply_stream` helpers +
  free `completion_activity` to satisfy the complexity hard ceiling (apply was
  21, now under 15).

## Verification so far

- cargo test: 41 pass (9 new: 3 model steer, 6 backend steer/rollover).
- cargo fmt clean; clippy -D warnings clean.
- guardrails run 1: complexity fail on ui.rs::apply (fixed by split). Run 2:
  fail only on examples/steer_probe.rs::run (throwaway probe; delete file then
  rerun for green verdict). Coverage ratchet rose to ~42.7 vs baseline 39.5.

## In flight

- Live disposable probe completed and `examples/steer_probe.rs` deleted;
  guardrails re-run green (see Final verification).

## Final verification (2026-11-11)

- Live disposable probe PASSED then deleted: mid-stream steer → Redirected,
  same stream continued (531 deltas, one terminal Complete); busy steer
  accepted (Queued); idle steer fell through; reload showed the steer user row.
- Guardrails final verdict: all gates pass (unit, lint, ratchet, complexity;
  patch skipped). Coverage ratchet raised 39.5 → 42.6 in guardrails.json;
  complexity baseline inventory rewritten by the runner (13 entries).
- Acceptance wrap-up (jev): stamped [x] criteria 1,2,3,4,5,8 (noul .77–.97).
  Left open: clarify-cards-during-steers (0.44) and reconnect-no-replay
  (0.41) — no direct probe/test evidence; recommend covering both in
  /b-review or a follow-up native pass.

## Decisions

- Steer errors use operation "steer" so the live turn survives a failed steer;
  UI shows the notice only.
- Turn rollover keys on Terminal + differing non-empty message id; plain
  `message.complete` uses ensure_turn("") so it never rolls.

## b-review → b-iterate (2026-11-11)

- b-review verdict: Needs work (1 in-plan Critical). Findings in
  `iterate-steering.md`, now `status: completed`.
- Critical fixed: rollover was invisible to the UI (terminal until first
  delta → Enter clobbered the live rolled turn via plain submit, which can
  hard-interrupt). `ensure_turn` now returns `(&mut Turn, server_started)`;
  `message.start`/`message.delta` emit `Event::TurnRolled { runtime_id }`;
  `Conversation::turn_rolled()` restores Streaming from settled states only
  (busy/Stopping guarded). Covers the untracked-turn-after-reconnect case too.
- Warning fixed: history pages moved by value in `apply` (no per-page clone).
- Verification: fmt clean, clippy -D warnings clean, 43 tests pass
  (41 + 2 new). Release binary rebuilt at target/release/hermes-native.
- User report "can't send follow-up while running" was a stale release
  binary (built 11-10 20:31, before the steering edits), not a code bug.
- Out-of-plan (not in iterate file): guardrails functional gate advisory
  fail — remote_smoke first-inference model mismatch (grok-4.7), the known
  model-picker discrepancy tracked in backlog item phase-1-contract-proof.
