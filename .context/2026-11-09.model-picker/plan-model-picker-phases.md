---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [phasing, model-picker, providers, fuzzy-search, gtk, gateway]
source_plan: plan-model-picker.md
phases: 3
format: discrete
---
# Phased Plan: Searchable provider/model picker

> Derived from [plan-model-picker.md](plan-model-picker.md)

## Overview

- **Total phases**: 3
- **Rationale**: The live gateway contract is a blocking unknown; isolate disposable proof before backend/state changes, then build the native UI and verify end to end.
- **Estimated total effort**: 3 focused sessions.
- **Difficulty mix**: 1 hard, 1 hard, 1 hard.

## Phase Summary

| Phase | Status | Difficulty | omp_execution | File |
|-------|--------|------------|---------------|------|
| 1: Prove session switching contract | pending | hard | none | [phase-1-contract-proof.md](phase-1-contract-proof.md) |
| 2: Implement model state and backend | pending | hard | none | [phase-2-model-backend.md](phase-2-model-backend.md) |
| 3: Build picker and verify acceptance | pending | hard | none | [phase-3-picker-acceptance.md](phase-3-picker-acceptance.md) |

## Dependency Matrix

| From → To | Type | Reason |
|-----------|------|--------|
| Phase 1 → Phase 2 | HARD | Blocking assumptions A-1 and A-2 must prove that prime-then-switch controls first inference; failure requires stop-and-replan. |
| Phase 2 → Phase 3 | HARD | The picker depends on the catalog, per-chat states, backend commands/events, and fuzzy filtering implemented and tested in Phase 2. |

## Dependency Diagram

```text
Phase 1 ──→ Phase 2 ──→ Phase 3
```

**Legend:** `──→` = HARD dependency (blocking)

**Dependency details:**
- Phase 2 HARD-depends on Phase 1: A-1/A-2 validate the only planned safe switch path for empty sessions. A failed probe invalidates implementation assumptions and requires replanning.
- Phase 3 HARD-depends on Phase 2: UI integration requires stable model/backend contracts and fuzzy results.

## Parallel Opportunities

None. Each phase produces a required contract or interface for the next.

## Execution Workflow

Use this overview as the durable navigation map for an OMP execution session. For each phase:
1. Read the first non-completed phase from the Phase Summary table.
2. Read that discrete phase file and execute only its scope using the listed `buck_hint`.
3. If the phase's `omp_execution` is `orchestrate | workflow`, drop the matching keyword on the first turn before the build command. If it is `goal`, run `/goal set "<plan User Goal>" --budget <omp_goal_budget>` first instead. Either way, see the phase file's "Per-Phase Execution Loop" for the precondition.
4. Run `/b-review` against the phase file after implementation.
5. If review creates an `iterate-*.md` artifact (in-plan issues), run `/b-iterate`, then re-run `/b-review`. Out-of-plan findings route to a separate `/b-plan` → `/b-build` cycle. If review flags documentation impact, run `/b-docs` before `/b-save`.
6. Run `/b-save` so memory, draft commits, phase state, and review/iteration artifacts are durable.
7. Run `/b-commit` before moving to the next phase.
8. If interrupted mid-cycle, leave the phase `in-progress` and resume it next turn.

**Commit invariant:** one phase completion equals one commit; do not batch completed phases.

## Execution Checklist

- [ ] Phase 1: Prove session switching contract — probe → review → save → commit
- [ ] Phase 2: Implement model state and backend — build → review → iterate if in-plan issues → docs if doc impact → save → commit
- [ ] Phase 3: Build picker and verify acceptance — build → review → iterate if in-plan issues → docs if doc impact → save → commit
