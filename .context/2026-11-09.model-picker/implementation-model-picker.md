---
status: active
date: 2026-11-10
subject: 2026-11-09.model-picker
topics: [model-picker, implementation, verification]
related: [plan-model-picker.md, research-prime-contract-2026-11-09.md]
---
# Model picker implementation

## What shipped

`src/model.rs` holds the shared layer: `ModelIdentity` (provider + model, with `switch_value` for the scoped switch and `prime_value` for the provider-less prime), `Catalog` parsed from `model.options` with canonical `custom:` provider tokens, a dependency-free normalized multi-token subsequence scorer, and `ModelState` (`Unknown`, `Confirmed`, `Switching`, `Queued`, `Unresolved`) with its transitions on `Conversation`.

`src/backend.rs` fetches the catalog once per connection, captures create/resume model metadata, runs prime → switch for a chat that has never run, switches directly otherwise, validates `scope == "session"` and `confirm_required`, maps `deferred` to the queued state, treats a timeout as ambiguous, and emits `Event::ModelObserved` from `message.complete.payload.usage.model`.

`src/ui.rs` adds the composer-bottom trigger (model + provider on two lines) and a popover with `SearchEntry` and a bounded `ListView`. Rows keep provider and model together, mark the current pair, and label an unrepresentable name as unavailable. Arrow keys walk results, Enter activates, Escape closes and returns focus to the editor, and Enter in the picker never sends. `confirm_required` raises an `adw::AlertDialog` and only an accepted confirmation repeats the target with `confirm_expensive_model: true`.

`resources/style.css` gained picker rules. `docs/architecture.md` documents the behavior and the effective-model rules.

## Contract detail found by implementation

The prime must be **provider-less**. `config.set` skips waiting for a building agent when the value names an explicit provider, which is exactly the acknowledged-but-ineffective path; `<model> --session` waits for the build, and the following `<model> --provider <provider> --session` then applies to it. The first smoke run failed with this bug and passed after the fix.

A catalog row is advertised, not guaranteed: one Fireworks row was rejected by its own provider ("model was not found in this provider's listing"). The rejection surfaced as a normal `ModelRejected` and left the selection unchanged, which is the intended behavior.

A chat that never ran persists no server row, so `session.delete` answers `4007 session not found`; the smoke treats that as already cleaned up.

## Verified

- `cargo test --lib`: 29 passed, including catalog identity, provider-prefix canonicalization, unrepresentable tokens, ranking/stability, and the four model-state transitions.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`: clean.
- `cargo build --release`: clean.
- Live remote smoke (`cargo run --example remote_smoke -- --trusted-tunnel`): `connected created=3 turn1=lattice turn2_context=true second=marble switched=true reconnect_history=true model_switch=deepseek-flash`. It proves the catalog, the provider-less prime, the scoped switch, that the first inference reported the chosen model, and that the server default did not move; all three fixtures were deleted.
- Native GTK, real input on the running release build: the trigger renders "Choose a model" + "server default"; the popover opens with the search placeholder; typing `deepseek` filtered rows; `deepseek-flash` listed under "Multi-Model Gateway"; Enter selected it and the trigger then showed `deepseek-flash` + `Multi-Model Gateway` with no message sent; Escape closed the picker.

## Not verified, and one contract correction

- The 640×480 window layout. Hyprland's Lua `resize` dispatcher rejected every size.
- The queued "`· next turn`" label on screen. The gateway behavior is proven by the mid-stream disposable probe; the label text is unit-tested.
- Keyboard arrow navigation inside the results list. Search, Enter, and Escape were exercised.
- A later `remote_smoke` run failed: after prime-then-switch the first inference reported `grok-4.7`, not the chosen model. An empty chat's acknowledgement is therefore not confirmation. The client keeps "Switching to …" until `usage.model` arrives, and a different reported model replaces the selection.

## Incidental

Driving the app required moving windows on the user's desktop: the ChatGPT window and the app were moved to workspace 3, and the app was left open, floated, and maximized. One chat titled `Test: say, "hello"` (`20261110_204403_b67fc1`, 4 messages) was created during the GUI run; it was left in place because its content cannot be attributed with certainty.
