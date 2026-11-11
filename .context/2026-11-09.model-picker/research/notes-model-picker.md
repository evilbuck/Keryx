---
status: completed
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, hermes, gtk, fuzzy-search]
---
# Model picker research notes

## Requested outcome
Add a model picker below the message editor, inside the chat composer, with quick fuzzy filtering and both provider and model visible. Planning only; no application changes authorized in this session.

## Existing client evidence
- `src/ui.rs:287-318`: the composer is a vertical GTK box with an editor and a bottom horizontal action row (shortcut hint, Stop, Send). The picker belongs in this bottom area without covering the editor or moving the composer off its bottom anchor.
- `src/model.rs:168-219,265-278,416-425`: commands, created/resumed events, conversations and app state currently contain no provider/model selection or catalog.
- `src/backend.rs:579-640`: create sends only a title, resume sends the stored server ID, and submit sends only runtime session ID and text. No model contract is implemented.
- `src/ui.rs:713-763,1099-1125`: submit uses per-conversation drafts; busy/read-only/disconnected/resume-pending states already gate sending. Preserve those semantics, Stopping generations and explicit reconnect/no replay.
- `Cargo.toml`: GTK4 0.10 with 4.16 minimum, libadwaita 0.8 with 1.6 minimum; no fuzzy-search dependency. Reuse native controls and current list-store conventions.
- `.context/2026-11-08.hermes-native-client/integration-contract-current.md`: remote `.env` gateway is sole authority; keep credentials out of output. Native `(ssv)` actions require `tuple.to_variant()` if touched.

## Investigation underway
- Primary Hermes sources: catalog discovery, exact provider/model identity, active model and safe session-scoped model selection. Verify actual remote read-only routes before treating upstream as compatible.
- Official GTK/gtk-rs docs: popover/search/list composition and real fuzzy matching at the existing minimum versions.
- Scope preference: per-chat selection, no global Hermes config writes, no hardcoded external provider catalog, and no automatic prompt replay. Final contract depends on evidence.

## Planning environment
Buck capability: full. Probe source: active injected skill catalog; `b-build`, `b-review`, `b-save` present; no missing sentinels. Prior project context was supplied by recalled memories and the on-disk memory index. No active backlog item exists (items directory empty; no todo index).

## Live remote catalog probe

Read-only probe on 2026-11-09 against the configured remote `.env` gateway:

- Password login: HTTP 200; WebSocket ticket: HTTP 200; received `gateway.ready`.
- JSON-RPC `model.options` with `{}` succeeded. Result keys: `providers`, `model`, `provider`.
- The response advertised 18 providers and 485 provider/model rows. Counts are dated observations, never constants for implementation or tests.
- Provider rows include `slug`, `name`, `is_current`, `is_user_defined`, `models` (string IDs), `total_models`, `source`, `authenticated`, `free_tier`, `pricing_pending`, `unavailable_models`, `capabilities`, `featured_models`.
- All returned providers advertised `authenticated: true`; no unavailable entries were present. This is catalog metadata, not proof that every model can complete a paid request.
- Current provider was a user-defined provider: the top-level provider identity used a `custom:` prefix, but its provider row `slug` omitted that prefix. Selection identity must preserve Hermes' canonical provider rules, not assume the two strings are identical or infer a provider from a model ID prefix.
- No conversations were created/resumed/edited, no prompts were sent, and no model/global settings were changed. Cookies and ticket stayed in probe memory; credentials were not logged.

Contract and GTK source investigation is complete; see `notes-hermes-contract.md` and `notes-gtk-picker.md`. Successful session switching and native interaction are implementation acceptance work, not verified behavior from this planning session.

### Catalog scope check

`model.options` also returned the same catalog and reported selection when given a deliberately nonexistent `session_id`. Therefore that result is not evidence of a particular conversation's effective model. Do not label the catalog's top-level selection as a confirmed per-chat model; use an actual session-state read or acknowledged session-specific change.

## Completed synthesis

- Configured-only catalog parameters returned 16 providers / 413 provider-model rows.
- `config.set` with explicit `--session` and an intentionally nonexistent runtime reached its `4001` session guard without changing state.
- `session.info` is unsupported (`-32601`) on this remote; upstream helpers are not assumed available.
- GTK 4.16 needs a local fuzzy matcher; preserve native controls and the current minimums without a new dependency.
- Saved `../plan-model-picker.md`; registered one feature-level backlog item. Planning only; no code checks or native picker acceptance were run.
