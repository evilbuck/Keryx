---
status: completed
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, sources]
---
# Model picker sources

Access date: 2026-11-09.

## Local implementation and verified history
- `src/ui.rs:287-318,713-763,766-945,1099-1158`: native composer layout, prompt submission and control gating.
- `src/model.rs:168-278,416-463`: shared commands/events, conversation lifecycle and reconnect state.
- `src/backend.rs:527-686`: ready/command dispatch and current RPC parameters.
- `Cargo.toml`, `resources/style.css`: dependency minimums and existing native styling.
- `.context/2026-11-08.hermes-native-client/integration-contract-current.md`: preserved integration and safety invariants.
- `.context/memory/index.md`: implementation and remote-connection history references; relevant recall supplied in session.

External source evidence and credential-free live observations will be appended as research returns. Upstream schema alone is not proof of remote compatibility.

## Configured remote gateway (live primary source)

- Accessed 2026-11-09 through password login and a single-use WebSocket ticket. Host, credentials, cookies and ticket intentionally omitted.
- `model.options {}` returned successfully: `providers: [...]`, `model: string`, `provider: string`.
- Observed 18 provider records / 485 provider-model rows, including a current user-defined provider with a `custom:`-prefixed top-level provider identity and an unprefixed provider-row slug.
- Read-only catalog inspection only. Model switching, inference and session-persistence behavior have not been exercised.
- `model.options {session_id: <deliberately nonexistent ID>}` succeeded and reported the same selection. A successful catalog call does not validate or prove per-session active model state.

## Decisive external sources

Types: primary source repository and official documentation. Accessed 2026-11-09.

- https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/inventory.py — catalog, configured-only filtering and custom provider aliases.
- https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_complete.py — model.options and initialized-session/default distinction.
- https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_config_set.py — structured model switch, confirmations and session scope.
- https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/model_switch.py — provider routing and why explicit --session is required.
- https://docs.gtk.org/gtk4/enum.StringFilterMatchMode.html — exact/substring/prefix only; not fuzzy ranking.
- https://docs.rs/gtk4/0.10.1/gtk4/enum.StringFilterMatchMode.html — pinned Rust API corroboration.
- https://docs.gtk.org/gtk4/class.Popover.html — native popover/focus/dismissal.
- https://docs.gtk.org/gtk4/class.SearchEntry.html — native query and activation.
- https://docs.gtk.org/gtk4/class.ListView.html — native results and activation.

Full API/version and source evidence: [Hermes](notes-hermes-contract.md), [GTK](notes-gtk-picker.md).

## Final live guard observations

- Configured catalog parameters succeeded: 16 provider records / 413 provider-model rows.
- `session.info` for a nonexistent runtime returned `-32601` (unsupported).
- `config.set` for a nonexistent runtime with explicit `--session` returned `4001` (session not found). No switch/global configuration change occurred.
- All feature behavior beyond catalog discovery remains for disposable remote/native implementation verification.
