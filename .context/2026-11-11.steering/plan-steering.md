---
status: active
date: 2026-11-11
subject: 2026-11-11.steering
topics: [steering, prompt-submit, session-redirect, busy-turn, gtk-composer]
research: [research-gateway-steering.md]
iterations: []
spec:
memory: []
---

# Plan: Steering commands during an active response

## User Goal

The user can keep steering a conversation while Hermes is still responding:
the composer stays usable during a busy turn, pressing Enter dispatches the
steer immediately (multiple steers allowed), and the server folds it into the
live turn. Steering also works while a clarification/approval card is pending,
but never while Stopping.

## Goal

Lift the client-side busy-turn block on submission and dispatch busy submits
through the gateway's `session.redirect` steering RPC, without weakening the
verified invariants: interrupt generations, Stopping-as-ACK, explicit
reconnect without replay, and server-owned history.

## Context used / assumptions

- User decisions (2026-11-11 interview, see Light Grill):
  - Dispatch: **immediate**; the server decides how the steer joins the turn.
  - Stacking: **multiple** steers per turn, each with its own optimistic row.
  - Busy states: allowed during `Submitting | Streaming | AwaitingInput`;
    blocked during `Stopping` (unchanged invariant).
- Server contract (`.context/2026-11-11.steering/research-gateway-steering.md`,
  upstream v0.23.0 sources):
  - Busy `prompt.submit` is policy-dependent (`display.busy_input_mode`,
    default `interrupt`): text-only submit tries in-place redirect
    (`{status:"redirected"}`); if redirect is unavailable it **hard-interrupts
    the live turn** and FIFO-queues (`{status:"queued"}`). That hard-interrupt
    fallback kills the running response — not what a steer should do.
  - `session.redirect {session_id, text}` is the canonical TUI busy path:
    during a model request it aborts only that request and retries the same
    turn; during tool execution it degrades to a steer (no kill). Result:
    `redirected | queued | rejected`. `rejected` means "not running or
    inapplicable" — the TUI then queues a normal next prompt.
  - Redirect/steer do **not** end the turn: no `message.complete` at accept,
    same `turn_id`, further `message.delta`, one terminal
    `message.complete {status: complete|interrupted|error}` per turn. There is
    no `message.error` event.
  - `session.redirect` params are `{session_id, text}` (+ optional `profile`);
    schema is `additionalProperties: false`, so the TUI's extra
    `execution_generation` field must NOT be sent by this client.
  - Clarify/approval pending does not block submission: the turn stays
    `running`, so the busy policy applies.
  - `session.interrupt` drops the server FIFO and cancels open cards; queued
    user rows already persisted stay in history but never run.
  - A FIFO-drained follow-up starts a **new** server-initiated turn (new
    `message.start`) after the live turn settles — with no client submit
    involved.
- Client facts (read 2026-11-11):
  - `src/ui.rs` `submit()` (≈995) gates on `send.is_sensitive()`;
    `refresh_controls` (≈1644) disables Send whenever `c.status.busy()`.
  - `src/model.rs` `TurnStatus::busy()` (≈128) = `Submitting | Streaming |
    AwaitingInput | Stopping`; `submit_acknowledged` (≈691) opens only from
    `Submitting | Idle`.
  - `src/backend.rs` `Command::Submit` (≈649) drops submits during `Stopping`
    and **overwrites** `self.turns.insert(runtime_id, …)` with a new
    generation — safe for idle submits, but a busy steer must not clobber the
    live turn's tracking. `retire_submit_jobs` / `retire_turn_jobs` (≈1334)
    key jobs by runtime id.

### Chosen dispatch design

Busy Enter → `session.redirect {session_id, text}`:

- Matches the upstream canonical TUI default (`busy_input_mode: interrupt`).
- Never hard-interrupts the live response (pure `prompt.submit` on busy can,
  via its interrupt+queue fallback).
- `rejected` → client immediately retries as `prompt.submit {session_id,
  text, queued: true}` (queue-only, never interrupts; on an idle session the
  busy loop falls through to the normal streaming path). Mirrors the TUI's
  "queue as the next prompt" fallback.
- Idle Enter → existing `prompt.submit` path unchanged.

## Light Grill

- Q: Send immediately vs queue locally until the turn completes? → resolved:
  immediate (recommended: immediate).
- Q: Multiple steers per turn or one at a time? → resolved: multiple
  (recommended: multiple).
- Q: Allow steering while clarification/approval pending? while Stopping? →
  resolved: allowed while awaiting input; blocked while Stopping
  (recommended: same).
- Q: Busy RPC — plain `prompt.submit` vs `session.redirect`? → resolved by
  research: `session.redirect` with queued-submit fallback, because plain busy
  `prompt.submit` can hard-interrupt the live turn (kills the response the
  user is steering).

## Assumptions Ledger

- A-1 — The deployed remote gateway (v0.23.0) accepts `session.redirect` from
  the dashboard canonical dispatch, as its source shows.
  status: deferred, blocking: false. validation_path: live disposable-
  conversation probe (Verification step 3) exercises the exact RPC before
  native reliance.
- A-2 — `prompt.submit {queued: true}` on an idle session falls through to the
  normal streaming path (source: busy-admission loop breaks when not running).
  status: deferred, blocking: false. validation_path: same live probe sends
  the fallback shape once against a settled disposable session.
- A-3 — A FIFO-drained new turn (server-initiated `message.start`, new message
  id) is routed by the existing delta/complete dispatch without a client
  submit. status: deferred, blocking: false. validation_path: backend unit
  test with an unsolicited second `message.start`/delta sequence, plus the
  live probe.

## Scope

- `Command::Steer` backend path: busy dispatch to `session.redirect`, accept
  handling (`redirected | queued`), `rejected` fallback to queued
  `prompt.submit`, error surfacing (4002/4009/4091-class).
- Model layer: steer-aware optimistic rows and status transitions that keep
  the live turn's state (never reopen `Stopping`, never claim `Submitting`).
- UI: Send/Enter enabled during `Submitting | Streaming | AwaitingInput`
  (still blocked during `Stopping` and while a resume reload is pending);
  activity text for steered/queued outcomes; multiple stacked steers.
- Server-initiated turn rollover: FIFO-drain `message.start` for a new message
  id rolls the backend `Turn` without a client submit and retires the prior
  submit jobs.

## Out of scope

- `prompt.btw` / `prompt.background` side-question RPCs; `subagent.steer`.
- Slash-command-style explicit `/steer` vs `/queue` affordances (one implicit
  busy path only).
- Images on steer (upstream refuses them on this control; composer is
  text-only).
- Any change to interrupt/Stopping semantics, reconnect policy, or history
  ownership. Model picker stays as-is.

## Affected files

- `src/backend.rs` — `Command::Steer`, redirect dispatch + fallback, accept
  events, turn rollover on unsolicited `message.start`.
- `src/model.rs` — steer optimistic rows, `steer_acknowledged` transitions,
  busy-submission rules.
- `src/ui.rs` — send sensitivity, `submit()` busy branch, activity display.

## Implementation steps

1. **model.rs — steer state transitions.** Add methods: record an optimistic
   user row for a steer while busy (existing `record_message`), keep current
   `TurnStatus` (no `Submitting`), and a `steer_acknowledged(kind)` where
   `redirected` refreshes streaming activity and `queued` sets a
   "Waiting for this response to finish…" activity. `Stopping` stays
   closed to steers; `submit_acknowledged` is untouched.
2. **backend.rs — steer command and dispatch.** Add `Command::Steer
   {runtime_id, text}`. In `handle`, when the runtime's turn phase is
   `Submitting | Streaming | AwaitingInput`, send `session.redirect
   {session_id, text}` (JobKind::Steer) — do **not** touch `self.turns`,
   generations, or submit jobs. Idle/absent turn → existing `prompt.submit`
   path (state race safety net). `Stopping` → drop, as today.
3. **backend.rs — result handling.** `redirected` → `Event::SteerAccepted`
   (kind Redirected); `queued` → `Event::SteerAccepted` (kind Queued);
   `rejected` → automatically send `prompt.submit {session_id, text,
   queued: true}` as a new submit job (normal generation bookkeeping);
   JSON-RPC/error codes surface like submit failures (`operation_failed`
   path) without ending the live turn unless the operation was `submit`.
4. **backend.rs — server-initiated turn rollover.** In the delta/complete
   routing, a `message.start`/first delta carrying a message id different
   from the tracked `Turn.message_id` while no client submit is pending rolls
   the `Turn` to the new message (fresh segment/offset), retires prior submit
   jobs, and keeps the interrupt generation chain intact (late frames of the
   old turn cannot affect the new one).
5. **ui.rs — controls and submit branch.** `refresh_controls`: Send sensitive
   when writable, no pending reload, draft non-empty, and
   `status != Stopping` (busy is no longer a blocker). `submit()`: when the
   conversation is busy (and not Stopping), record the optimistic row, clear
   the draft, and dispatch `Command::Steer`; otherwise the existing path.
   Activity label renders steer outcomes; the clarify/approval card stays
   visible and answerable while steers are sent.
6. **Tests.** Model: steer submit allowed per status, Stopping blocked,
   steer-ack transitions, optimistic rows do not move the history cursor.
   Backend: busy dispatch chooses `session.redirect`; `rejected` triggers the
   queued fallback; accept events do not alter generations; unsolicited
   second `message.start` rolls the turn; late interrupt of the old turn is
   rejected after rollover (invariant); Stopping drop retained.
7. **Live remote probe (disposable conversation).** Verify A-1–A-3: steer
   mid-stream (`redirected`), steer during tool execution (degrades to
   steer/queue), `rejected` fallback shape, stop-while-queued drops the
   follow-up, history after reload matches server rows. Delete the fixture.
8. **Guardrails.** `cargo fmt --all -- --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `npm run guardrails:check` (coverage/complexity per guardrails.json v2).

## Acceptance criteria

- [x] Composer and Send are usable during `Submitting`, `Streaming`, and
      `AwaitingInput`; each Enter dispatches immediately; multiple steers each
      produce their own user row; draft clears per steer.
- [x] Busy Enter sends `session.redirect {session_id, text}` (no extra
      fields); the live turn's status/generation tracking is untouched; a
      `redirected` steer continues the same stream with no terminal frame.
- [x] `rejected` results in one automatic `prompt.submit {queued: true}`; no
      duplicate optimistic row.
- [x] `Stopping` blocks steers exactly as it blocks submits today; a queued
      steer dropped by `session.interrupt` is reflected only via activity
      text — no local history invention.
- [x] A FIFO-drained follow-up (new `message.start`, no client submit)
      renders as a continued conversation with correct turn rollover.
- [ ] Clarify/approval cards remain answerable while steers are in flight;
      late-card rejection rules unchanged.
- [ ] Reconnect still never replays steers or prompts; server history remains
      authoritative after reload.
- [x] Guardrails green (fmt, clippy `-D warnings`, tests, coverage/
      complexity contract).

## Verification

1. `cargo test` — new model/backend unit tests above, plus the existing 25
   (no regressions to Stopping/interrupt/history tests).
2. Live disposable-conversation probe against the configured remote `.env`
   (never existing chats; delete fixture after) — exercises the exact RPC
   shapes from A-1–A-3 and the stop-drops-queue behavior.
3. Native Wayland manual pass on the release build: steer mid-stream, stacked
   steers, steer with a card open, Stop during a steered turn, reload
   history.
4. `npm run guardrails:check` — durable contract verdict `pass`.

## Risks

- **Deployed gateway diverges from v0.23.0 source** (e.g. rejects
  `session.redirect` with 4010): fallback still works via queued submit;
  worst case the steer queues instead of redirecting. Mitigated by the live
  probe before native reliance (A-1).
- **Optimistic-row duplication on history reload** for queue-path rows
  (server persists the user row at accept): the existing optimistic-vs-
  server-row reconciliation must be checked in the probe; if duplicates
  appear, dedupe by content/order at render time, never by editing server
  data.
- **`message.complete.text` may be `""` after an interrupt sentinel** while
  deltas were rendered: existing `finish_message` keeps prior text — no
  change needed, covered by an existing-pattern test.
- **Redirect during a clarify wait** may degrade to steer/queue (upstream
  timing unverified): acceptable outcome, no client crash; probe covers it.

## Execution Instructions

This is a non-phased execution-ready plan. Treat the whole plan as one unit:
1. Run `/b-build` against this plan.
2. Run `/b-review` against this plan.
3. If review creates an `iterate-*.md` artifact (in-plan issues), run
   `/b-iterate`, then re-run `/b-review`. Out-of-plan findings route to a
   separate `/b-plan` → `/b-build` follow-up. If `/b-review` flags
   documentation impact, run `/b-docs` before `/b-save`.
4. Run `/b-save`, then `/b-commit`.
