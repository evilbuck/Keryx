---
status: pending
phase: 3
order: 3
plan: plan-model-picker.md
phases_overview: plan-model-picker-phases.md
difficulty: hard
model_hint: Strong reasoning model; native GTK accessibility, keyboard behavior, remote acceptance, and regressions.
buck_hint: /b-build-hard
goal: Deliver the native composer picker and prove end-to-end remote and Wayland behavior.
files: [src/ui.rs, resources/style.css, examples/remote_smoke.rs, docs/architecture.md]
from_plan_steps: [4, 5]
depends_on: [2]
dependency_type: HARD
acceptance_criteria:
  - "[ ] Native picker sits below the editor, shows provider and model, and fits 640×480."
  - "[ ] Search, stable results, refresh/loading/error/no-match states, confirmations, keyboard flow, focus restoration, and accessible labels meet the parent plan."
  - "[ ] Empty-chat first inference, initialized and busy switching, reconnect/resume, and per-chat isolation pass against the configured remote, following Phase 1 evidence."
  - "[ ] Composer Enter/Shift+Enter, drafts, stopping, approvals/clarifications, history paging, and tool summaries remain intact."
  - "[ ] Guardrails, focused tests, format, strict Clippy, release build, remote smoke, and native Wayland acceptance pass; fixtures are deleted and scanned."
  - "[ ] Architecture docs describe only behavior proven by implementation and acceptance evidence."
completed_at: null
completed_by: null
---
# Phase 3: Build picker and verify acceptance

## Context

Parent goal: add a composer-bottom provider/model picker with quick fuzzy search, showing provider and model; new chats start with the default, and switching is available at any time. Phase 2 supplies the tested catalog, session-state contract, backend commands/events, and fuzzy filtering.

## Implementation Details

1. In `src/ui.rs`, build the GTK4 MenuButton/Popover/SearchEntry/ListView picker below the message editor.
2. Show provider and model in collapsed/expanded views. Include bounded results, current pair, loading/no-match/error states, explicit refresh, and `confirm_required` confirmation.
3. Apply evidence-based gating: no selection while another switch is pending or ambiguous; busy selection is queued with an explicit next-turn label only if Phase 1 proves deferred semantics, otherwise disable it.
4. Ensure picker Enter never submits; Escape restores composer focus/draft; keyboard-only navigation/select/dismiss and accessible labels work. Add small theme-consistent CSS.
5. Update `examples/remote_smoke.rs` for disposable switching/resume and event handling; use only the configured remote and clean every fixture.
6. Update `docs/architecture.md` with the implemented, remotely verified effective-state rules.
7. Exercise native Wayland acceptance including 640×480 layout, keyboard-only use, reconnect/resume, empty and busy switching, and regression scenarios from the parent plan.

## Risks

- Focus or key handling can accidentally submit the composer or lose drafts; explicitly test Enter, Escape, and Shift+Enter.
- UI must not label unverified ACKs or lazy `model.options` as confirmed model state.
- Remote fixtures must be disposable and fully removed; never modify existing user chats.

## Verification

Complete parent plan acceptance criteria and run its deterministic guardrails contract. Verify the actual native GTK surface against the configured remote; record `usage.model`/resume evidence and fixture-cleanup scan. Run release build, focused tests, formatting, strict Clippy, and smoke acceptance. Do not report unexercised visual or remote criteria as passed.

## Per-Phase Execution Loop

1. Run `/b-build-hard` for this phase only.
2. Run `/b-review` against this phase file.
3. If review creates an in-plan `iterate-*.md`, run `/b-iterate`, then re-run `/b-review`; route out-of-plan issues to separate planning. Run `/b-docs` before `/b-save` if review flags documentation impact.
4. Run `/b-save`, then `/b-commit` to checkpoint this phase.
5. If incomplete, retain `status: in-progress` and resume here.
