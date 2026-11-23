---
date: 2026-11-22
domains: [build, rust]
topics: [gpui, rust-future-incompatibility, stacksafe]
related: []
priority: low
status: completed
---

# GPUI Rust compatibility

The reported release build finished successfully; the diagnostics were future-incompatibility warnings, not current compile failures.

- The grid repeat helper in `vendor/gpui/src/taffy.rs` relied on Rust's temporary `f32` fallback for the generic Taffy `length` and `fr` helpers. Explicit `f32` literals preserve the layout values. See [Rust #154024](https://github.com/rust-lang/rust/issues/154024).
- `cargo report future-incompatibilities --id 1` identified `proc-macro-error2` 2.0.1 publicly re-exporting private `proc_macro` ([Rust #127909](https://github.com/rust-lang/rust/issues/127909)). `cargo tree --locked -i proc-macro-error2` traced it through GPUI's `stacksafe` 0.1.4 dependency. `stacksafe` 1.0.2 is the first release whose matching macro crate drops that dependency; use the published fix rather than a local fork or warning suppression.

Both lockfiles resolve `stacksafe` and `stacksafe-macro` to 1.0.3. `cargo build --release --locked --future-incompat-report` passed with zero future-incompatible warnings. The project guardrails passed, including the unit, functional, lint, coverage ratchet, and complexity gates.
