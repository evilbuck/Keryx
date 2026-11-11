# Designed for hyprland

Low overhead languages used only.

No electron or tauri


Research on the web before making a decision.

Don't write code that can be generated with a canonical generator.

<!-- BEGIN b-init-guardrails -->
# Quality Guardrails (managed block)

This block is managed by `b-init-guardrails`. Do not edit manually; re-run the skill to refresh.

## When to Run Guardrails Checks

The **mainline agent** owns dispatch. Run a guardrails check **at coherent points** — after a completed edit batch, never per-file. Examples:
- After finishing a feature or bug fix
- Before committing
- When the mainline agent is about to yield

If the session touched code, the check is **blocking** for completion. A session is docs-only when every changed path is `.md`, `.mdx`, `.txt`, `LICENSE`, or under `.context/`, `docs/`, or `presentations/` — those skip the gate with one line of explanation. Any other change (source, `package.json`, lockfiles, CI YAML) makes the session code-touching and the gate mandatory.

Do **not** run mid-edit; the working tree may be in an inconsistent state and yield false failures. `b-guardrails-check` only measures — it never dispatches itself and never edits.

## Deterministic Runner

The verdict is computed by one executable: `skills/b-guardrails-check/scripts/check.mjs`, invoked as `npm run guardrails:check`. `b-guardrails-check` and pull-request CI both call this runner; neither re-implements gate logic. It emits the verdict JSON on stdout and exits nonzero **only when a `required` gate fails** (exit 2 = no `guardrails.json`; exit 1 = malformed contract or required-gate failure).

## Enforcement States

Every gate carries an explicit state in `guardrails.json.enforcement` — `required` (failure blocks completion, exit 1), `advisory` (failure is reported, never blocks), `disabled` (not run). When the field is absent, the runner uses `DEFAULT_ENFORCEMENT` in `check.mjs`: unit/functional/patch/ratchet/complexity `required`, lint `advisory`. Promotion is explicit and monotonic (`disabled` → `advisory` → `required`): promote only after the runner is green in a clean CI environment; demotion or baseline weakening requires recorded user approval.

## How to Read a Verdict

`/b-guardrails-check` resolves its contract via `skills/b-guardrails-check/docs/contract-resolution.md` and returns the runner's structured verdict. `status: pass` — all gates passed, continue. `status: fail` — the verdict shows which gate failed and by how much. `contract` — `durable` means `guardrails.json` is authoritative. `ratchet_update` — a proposed update only; the check is read-only and the mainline agent applies approved baseline raises and inventory shrinkage at a coherent point.

## What to Do on Failure

**Unit / functional test failure**: fix the test or the code under test before committing. Never delete the test, never record `null` to silence the gate, never widen ignores.

**Lint gate failure**: fix the reported lint errors. Never widen the lint ignore config to silence the gate; if `lint_cmd` is genuinely wrong, re-run `/b-init-guardrails`.

**Patch gate below threshold**: if `required`, add tests for changed lines before committing; if `advisory`, report it and track follow-up. Never lower the threshold to go green.

**Global ratchet failure** (coverage below baseline): add tests to recover. Re-baseline only with explicit user approval.

**Complexity violation** (new function > 10 cyclomatic): refactor before committing, or add to the baseline via explicit re-baseline (manual opt-in).

## Contract Resolution

1. `guardrails.json` at repo root → authoritative (`contract: "durable"`).
2. Managed block present but `guardrails.json` missing → warn the contract is broken, continue to step 3.
3. `b-init-guardrails`' `scripts/detect-stack.ts` reports ≥ 1 ecosystem → ephemeral contract; run lint and test gates only.
4. No ecosystem detected → surface any `README.md` testing command block as unverified suggestions.
5. Nothing found → warn and offer `/b-init-guardrails`.

## Coherent-Point Dispatch Rule

Dispatch checks only after a completed edit batch. If a check fails: verify the working tree is consistent, re-run; if it fails again, treat it as a real failure.

<!-- END b-init-guardrails -->
