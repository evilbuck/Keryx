---
status: pending
phase: 1
order: 1
plan: plan-model-picker.md
phases_overview: plan-model-picker-phases.md
difficulty: hard
model_hint: Strong reasoning model; remote protocol behavior and safe fixture cleanup are failure-sensitive.
buck_hint: /b-build-hard
goal: Prove disposable-session prime-then-switch behavior before dependent implementation.
files: [examples/remote_smoke.rs]
from_plan_steps: [1]
depends_on: []
dependency_type: NONE
acceptance_criteria:
  - "[ ] A-1: prime-only disposable chat retains the remote default, evidenced by first inference usage.model."
  - "[ ] A-2: prime then canonical provider-scoped switch controls the first inference; failed proof stops execution and triggers replanning."
  - "[ ] A-3: busy-time switch result is observed; deferred behavior is verified or documented as unsupported for busy-disabled UI."
  - "[ ] Initialized switch, cold resume, per-chat isolation, and unchanged global model are verified on disposable chats."
  - "[ ] All created fixtures are deleted and a full session-list scan confirms cleanup."
completed_at: null
completed_by: null
---
# Phase 1: Prove session switching contract

## Context

Parent goal: add a composer-bottom provider/model picker with quick fuzzy search, showing provider and model; new chats start with the default, and switching is available at any time. The plan's safe empty-chat approach depends on gateway behavior not yet proven on the deployed remote.

A-1 and A-2 are blocking and belong here because the disposable live probe is the earliest direct validation path; A-3 is non-blocking but its queued-versus-disabled outcome determines later busy-state behavior.

## Implementation Details

1. Read the current plan and gateway research; use only the configured remote `.env` gateway, never local Hermes.
2. Run the disposable probe arms in plan step 1: prime-only control, prime+switch, initialized between-turns regression, busy switch, cold resume, isolation, and global-default before/after.
3. Use canonical `custom:<slug>` provider tokens and `--session` only. Treat ACK as insufficient; verify with `usage.model` and resume fields. Never use lazy `model.options` as effective-state proof.
4. Stop and replan if prime+switch fails. Do not implement an alternate or pretend-confirmed state.
5. Delete every disposable conversation and verify with a full session-list scan. Preserve credentials and avoid logging secrets.
6. Add/adjust the remote smoke scenario only as needed to make this proof repeatable.

## Risks

- Deployed gateway behavior may differ from the source diagnosis; failed A-1/A-2 blocks dependent implementation.
- Global model drift is consequential; require session scope and compare global identity before/after.
- Busy deferred semantics may be unsupported; record the evidence and let later UI disable busy switching.

## Verification

Run the disposable remote probe. Evidence must include actual inference `usage.model`, cold-resume identity, global default unchanged, and fixture cleanup scan. Do not claim success from ACKs or lazy catalog metadata.

## Per-Phase Execution Loop

1. Run `/b-build-hard` for this phase only; this is a remote contract probe, not a code feature.
2. Run `/b-review` against this phase file.
3. If review creates an in-plan `iterate-*.md`, run `/b-iterate`, then re-run `/b-review`; route out-of-plan issues to separate planning. Run `/b-docs` before `/b-save` if review flags documentation impact.
4. Run `/b-save`, then `/b-commit` before Phase 2.
5. If incomplete, retain `status: in-progress` and resume here.
