---
status: completed
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, hermes, gtk, fuzzy-search]
informs: [plan-model-picker.md]
---
# Research: native Hermes model picker

## Conclusion
Use the configured Hermes `model.options` catalog, session-only `config.set` for selection, and a native GTK search popover with local fuzzy ranking. No new dependency, GTK minimum bump, global model setting or third-party model catalog is needed.

## Live remote evidence
Authenticated read-only/guard-only probes on 2026-11-09 against the configured `.env` remote:

| Call | Result |
| --- | --- |
| Password login / WebSocket ticket / ready event | HTTP 200 / 200 / received |
| `model.options {}` | 18 provider records, 485 provider-model rows |
| `model.options {explicit_only:true,include_unconfigured:false,refresh:false}` | 16 provider records, 413 provider-model rows |
| Catalog with nonexistent runtime ID | Falls back to the same reported defaults; not per-session state proof |
| `config.set` with nonexistent runtime ID and explicit `--session` | `4001`, session-not-found guard |
| `session.info` with nonexistent runtime ID | `-32601`, unknown method; do not depend on it |

Counts are observations, not implementation constants. Catalog metadata does not prove every advertised model can complete inference. No conversation/model/global setting was changed and no prompt was sent. Socket closed; credentials, cookies, ticket and host omitted.

## Hermes contract and hazards
- Use configured-only catalog parameters; providers expose display names, slugs, opaque model IDs, authentication/availability metadata and optional aliases. Preserve provider/model pairs: identical model IDs under different providers are separate options.
- Current custom-provider identity used `custom:<slug>` while its catalog row omitted the prefix. Normalize documented aliases/prefixes for matching, not routing from model names.
- Existing-chat switch: `config.set {session_id:<runtime>,key:"model",value:"<model> --provider <provider> --session",confirm_expensive_model:false}`. Explicit `--session` prevents default-dependent global persistence. Never use global HTTP `/api/model/set` for the composer.
- Inspect scope, resolved value, errors, warnings and server-required confirmation. Disable switching while busy rather than supporting deferred switching.
- Effective state can come from verified create/resume metadata and `model.options` for a valid initialized runtime. Absent/lazy runtime IDs fall back to defaults; unsupported `session.info` is not a fallback.
- Version-tagged Hermes source describes session override persistence/resume. Actual successful switching, custom-provider routing, lazy-session behavior and resume persistence require the first implementation step's disposable remote smoke. They are not claimed verified here.

See [full Hermes evidence and source URLs](research/notes-hermes-contract.md).

## Native UX and fuzzy search
`MenuButton → Popover → SearchEntry + bounded ListView` fits existing GTK 4.16/gtk-rs 0.10 conventions. Put provider/model at the bottom-left of the composer, Stop/Send at the right, and move the shortcut hint to the preceding line for narrow windows. Ellipsize long labels and prefer opening upward.

GTK StringFilter supports exact/substring/prefix matching, not fuzzy scoring. Use a small dependency-free case-insensitive, separator-normalized multi-token subsequence scorer with stable ranking. Cache row normalization and compute scores once per query. No hardcoded model aliases or typo-correction feature.

Native focus/key handling still needs actual Wayland acceptance: Up/Down, Enter, Escape, no hover commit or leaked prompt submit, accessible labels, composer focus/caret restoration and minimum-size layout. See [GTK evidence and primary documentation](research/notes-gtk-picker.md).

## Preserved constraints
Server-owned IDs/history, per-chat drafts, remote-only validation, Stopping/interrupt generations, `(ssv)` GVariant boxing and explicit reconnect/no prompt replay remain unchanged. No application implementation was performed.

## Source trail
- [Plan](plan-model-picker.md)
- [Rolling notes](research/notes-model-picker.md)
- [Primary sources](research/sources-model-picker.md)
- [Prior integration contract](../2026-11-08.hermes-native-client/integration-contract-current.md)

Source types: official documentation, primary repositories, live remote API. Buck capability full; active injected skill catalog is the probe source.
