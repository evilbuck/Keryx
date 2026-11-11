---
status: pending
phase: 2
order: 2
plan: plan-model-picker.md
phases_overview: plan-model-picker-phases.md
difficulty: hard
model_hint: Strong reasoning model; cross-file Rust state, request correlation, and interrupt/reconnect invariants.
buck_hint: /b-build-hard
goal: Implement truthful per-chat model state, remote catalog/switching, and deterministic fuzzy filtering.
files: [src/model.rs, src/backend.rs]
from_plan_steps: [2, 3]
depends_on: [1]
dependency_type: HARD
acceptance_criteria:
  - "[ ] Catalog and per-conversation model identities preserve provider distinctions and canonical custom: tokens."
  - "[ ] Prime-then-switch uses session scope, validates ACK scope/deferred/confirmation, and represents confirmed, queued, switching, or unresolved states truthfully."
  - "[ ] Replies are correlated by stored/runtime/connection identity; stale replies cannot mutate another chat or reconnect state."
  - "[ ] Dependency-free fuzzy filtering has deterministic multi-token ranking and preserves same-name cross-provider rows."
  - "[ ] Focused behavior tests cover identity, scoring, switch transitions, stale replies, busy gating, and ACK scope; existing invariants remain intact."
completed_at: null
completed_by: null
---
# Phase 2: Implement model state and backend

## Context

Parent goal: add a composer-bottom provider/model picker with quick fuzzy search, showing provider and model; new chats start with the default, and switching is available at any time. This phase proceeds only after Phase 1 validates blocking assumptions A-1 and A-2. A-3's result determines whether busy switching is queued or disabled.

## Implementation Details

1. Before exported Rust type changes, use language-server references and migrate all callers/tests.
2. In `src/model.rs`, add catalog/provider/model identities, per-chat selection lifecycle, commands/events, and the dependency-free normalized multi-token subsequence scorer. Keep provider/model pairs distinct and stable.
3. In `src/backend.rs`, fetch configured catalog per connection; capture create/resume model metadata; implement lazy prime → switch and initialized switch with request/runtime/connection correlation.
4. Validate session scope, `deferred`, and `confirm_required`; retain previous confirmed selection on definite reject/cancel; mark ambiguous transport outcomes unresolved and never silently retry.
5. Follow Phase 1's busy-switch evidence. Preserve explicit reconnect/no prompt replay, interrupt generations, server-owned IDs/history, and all GTK Variant boxing/dispatch invariants.
6. Add focused tests for consumer-visible behavior and edge transitions.

## Risks

- Incorrect event correlation can apply model state to another conversation; test stale replies across chat and connection changes.
- ACKs are not proof of effective model; only Phase 1's demonstrated contract and subsequent UI-visible state may confirm selection.
- Existing interrupt, no-replay, and server-owned history invariants are critical; avoid adjacent refactors.

## Verification

Run focused model/backend tests, `cargo fmt --all -- --check`, and strict Clippy for the changed code. Run `/b-guardrails-check` at the coherent completed batch. Confirm probe results remain represented without exposing secrets.

## Per-Phase Execution Loop

1. Run `/b-build-hard` for this phase only.
2. Run `/b-review` against this phase file.
3. If review creates an in-plan `iterate-*.md`, run `/b-iterate`, then re-run `/b-review`; route out-of-plan issues to separate planning. Run `/b-docs` before `/b-save` if review flags documentation impact.
4. Run `/b-save`, then `/b-commit` before Phase 3.
5. If incomplete, retain `status: in-progress` and resume here.
