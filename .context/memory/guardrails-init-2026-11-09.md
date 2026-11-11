---
date: 2026-11-09
domains: guardrails, testing
topics: guardrails, coverage, complexity, clippy, functional-tests
related: [hermes-native-implementation-2026-11-09.md]
priority: medium
status: active
subject: 2026-11-08.hermes-native-client
artifacts: [guardrails.json, .guardrails/complexity-baseline.json, AGENTS.md]
---

# Guardrails initialization (hermes-native)

Durable contract installed via /b-init-guardrails (v2 schema):

- Gates: unit `cargo test` required; functional `cargo run --release --example remote_smoke -- --trusted-tunnel` **advisory** (live remote server, disposable chat); lint `cargo clippy --all-targets -- -D warnings` advisory with `baseline_lint_clean: true`; patch gate **disabled** (no resolvable compare branch); global ratchet required; complexity gate required.
- Coverage runs via `.guardrails/coverage.sh` (env prefix breaks the runner's argv parsing; see gotchas). Complexity hotspots keyed `file::function` (short fn name) — lizard CSV columns: 1=CCN, 6=file, 7=short fn name.
- Baselines measured 2026-11-09: coverage 39.5% (lcov LH/LF summed across sections), lint clean (clippy + fmt), complexity inventory 14 functions > CCN 10 stored in `.guardrails/complexity-baseline.json` (split pointer). Hard-ceiling (>15) hotspots: `ui.rs connect_actions` 24, `ui.rs apply` 20, `examples/remote_smoke.rs run` 23, `ui.rs refresh` 17, `markdown.rs nested_lists_quotes_and_tables_keep_boundaries` 17.

Environment gotchas:
- `/usr/bin/cargo` (distro rustc) ships no llvm-tools; even `rustup component add llvm-tools-preview` is insufficient because `cargo-llvm-cov`'s autodetection fails on this box — it works only with `LLVM_COV`/`LLVM_PROFDATA` pointed at `~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin` and the rustup cargo first on PATH. Encapsulated in `.guardrails/coverage.sh` (runner parses argv, cannot take env prefixes).
- Coverage percentage must be summed across all `LF:`/`LH:` sections of the lcov file, not read from the last section.
- lizard CSV complexity column is index 1 (CCN); parse with a real CSV reader (function signatures contain commas). Runner baseline keys are `file::function` where file is CSV column 6 (stripped of `./`) and function is column 7 (short name).
