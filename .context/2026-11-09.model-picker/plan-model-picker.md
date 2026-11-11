---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, providers, fuzzy-search, gtk, gateway, lazy-initialization]
research: [research-model-picker.md, research-contract-smoke-2026-11-09.md, research-create-override-2026-11-09.md, research-switch-2026-11-09.md, research-lazy-switch-diagnosis.md]
iterations: []
memory: []
---
# Plan: searchable provider/model picker with truthful session switching

## User Goal
“I want to add a model picker from the available models from hermes in the chat interface. At the bottom of the new message box there should be a model picker with a quick filter fuzzy search. It should show both the provider and the model.” Later instructions: new chats use the default model; allow switching at any time.

## Goal
Choose the provider/model for the current conversation from the configured remote Hermes catalog, directly beneath the message editor. The picker must be truthful about which model the next message will use in every chat state, including empty lazy chats. Model changes affect this conversation only, never Hermes' global default. Switching is allowed whenever a chat is selected: idle empty chats, initialized chats, and busy chats (queued for the next turn with an explicit label).

## Context used / assumptions
- Prior plan and its no-go gate: a pre-first-turn `config.set` ACK did not switch inference; switching an initialized session did.
- Diagnosis artifacts in this folder, especially `research-lazy-switch-diagnosis.md`: the canonical `custom:` provider token also fails pre-first-turn, removing the provider-token confounder. Upstream v0.23.0 source shows `config.set` skips waiting for an in-progress agent build when a provider is explicit, while agent construction snapshots the model override separately.
- Upstream `_set_model` starts and waits for the agent build when `agent` is `None` and no explicit provider is supplied (`methods_config_set.py`). This is the client-side lever: a harmless "prime" call forces initialization, then the real provider-scoped switch applies to the live agent.
- Verified remote behaviors: `session.create` model/provider override; initialized `config.set` switch with `custom:` tokens and `--session`; cold-resume fields `model`/`provider` are truthful while lazy `model.options` is not.
- User decisions: default model for new chats; switching allowed at any time; do not replace existing empty sessions (server-owned IDs preserved).
- Remote-only validation against the configured `.env` gateway; local Hermes is never a target. Durable `guardrails.json` v2 now exists.

## Decision Closure
- **Selected course:** client-side two-step switch (prime → switch) plus truthful state display; no gateway patch required, no session replacement. Busy-turn switching uses the gateway's native deferred stash with an explicit "next turn" label. Upstream fix remains a documented follow-up, out of scope here.
- **Evidence:** live probes (empty-chat failure with canonical tokens; initialized success; resume-field truthfulness) and version-tagged upstream source paths cited in the diagnosis artifact.
- **Excluded scope:** global/server settings, provider setup, arbitrary model entry, favorites/recents, pricing UI, recreating empty sessions, upstream gateway patch, local Hermes.
- **Next action:** run the step-1 disposable prime+switch contract probe; a protocol mismatch is stop-and-replan, never permission to fake state.

## Assumptions Ledger
| id | statement | status | blocking | evidence / validation_path |
| --- | --- | --- | --- | --- |
| A-1 | A provider-less `config.set` with the current default model on a lazy session starts and waits for the agent build without changing the model. | deferred | true | Step-1 probe: prime a disposable empty chat, assert first inference stays on default. |
| A-2 | A full `<model> --provider custom:<slug> --session` switch immediately after priming controls the first inference of a previously empty chat. | deferred | true | Step-1 probe: prime then switch, assert `usage.model` of first completion. |
| A-3 | `config.set` during a running turn returns `deferred: true` and applies at the next turn start. | deferred | false | Step-1 probe: switch mid-stream in a disposable chat; if unsupported, fall back to busy-disabled switching and record it. |
| A-4 | `session.resume` response `model`/`provider` fields report the persisted per-chat selection. | validated | false | Prior cold-resume probe: resume fields and next inference agreed. |
| A-5 | Canonical `custom:<slug>` provider tokens are required in the switch value; quoted tokens are rejected. | validated | false | Prior probes: quoted 5001 rejection; canonical token accepted. |

## Material Risks
| failure_mode | impact | mitigation | rollback_or_fallback | validation_path |
| --- | --- | --- | --- | --- |
| Prime call changes the model it claims to preserve | Empty chat silently leaves the default before the user's switch lands | Prime uses the just-read global default identity; probe asserts no change; only `--session`, never global | Session-scoped re-set of the original default pair | Step-1 control arm: prime-only disposable chat keeps `usage.model` = default |
| Gateway persists a switch globally despite `--session` | Other sessions/defaults change | Refuse any ACK without `scope: "session"`; never call `/api/model/set`; global readback in probe | Restore global default via dashboard only if drift is observed | Step-1 global `model.options` before/after comparison |
| Deferred busy-switch is dropped or applied late | UI claims "next turn" but behavior differs | Treat `deferred: true` as the only queued state; confirmations re-ask before stash; drop label on terminal event | Show unresolved state; user re-selects | Step-1 busy arm; focused test of queued→terminal transitions |
| Prime blocks 30s+ on slow agent builds | Picker appears frozen | Show a typed "switching" state; send blocked only while the outcome is unknown; cancel restores old state | Keep prior confirmed selection and draft | Step-1 timing observation; native acceptance of long-build state |
| Lazy `model.options` used as evidence | False confirmed state | Never read effective state from lazy options; use resume fields and `usage.model` | Honest unresolved/default label | Focused tests + acceptance scenario |

## Scope
- Composer-bottom provider · model picker with fuzzy search, states, keyboard access, and truthful per-chat state.
- Two-step switching for empty/lazy chats; live switching for initialized chats; deferred switching for busy chats.
- Catalog fetch per connection with explicit refresh; resume/create metadata as display truth.
- Disposable remote contract probe before dependent implementation; permanent focused tests; native Wayland acceptance.

## Out of scope
- Global default changes, provider setup, arbitrary model entry, favorites/recents, capability/pricing controls.
- Replacing or recreating existing empty server sessions.
- Patching the upstream gateway (recorded as follow-up: synchronize `config.set` with in-progress agent builds and persist pins without a live agent).
- Local Hermes, web runtimes, API-key server.

## Affected files
| File | Planned change |
| --- | --- |
| `src/model.rs` | Catalog/provider/model identities (canonical `custom:` tokens), per-conversation model state (confirmed / switching / queued-next-turn / unresolved), commands/events, pure fuzzy scoring, focused behavior tests. |
| `src/backend.rs` | Catalog request, two-step switch sequence with request/runtime/connection correlation, deferred handling, resume/create model metadata capture; all on `hermes-net`; turn generations and no-replay untouched. |
| `src/ui.rs` | Composer picker row (MenuButton/Popover/SearchEntry/ListView), states, gating, confirmation dialog, accessible labels, focus restoration. |
| `resources/style.css` | Small picker/result/empty-state rules reusing the existing theme. |
| `examples/remote_smoke.rs` | Consume new events; add disposable switching scenario (empty-chat prime+switch, between-turns, resume) with configured catalog models. |
| `docs/architecture.md` | Picker behavior, switching states, effective-state rules after implementation proof. |

Before changing exported Rust types, use language-server references and migrate every affected caller/test. No generated gateway client.

## Implementation steps
1. **Prove the prime+switch contract in disposable chats.** Arms: (a) empty chat prime-with-default then full switch → first inference uses target; (b) prime-only control stays default; (c) initialized between-turns switch (regression); (d) busy-time switch returns `deferred` and applies next turn (or record unsupported); (e) cold resume fields match next inference; (f) isolation and global unchanged before/after. Use canonical `custom:` tokens and `--session` only; delete all fixtures and verify by full session-list scan. Red baseline (switch-only on lazy stays old) is already recorded; do not re-run it. Stop-and-replan if (a) fails.
2. **Extend shared state and backend.** Catalog per connection; per-chat model state; `SetModel` runs prime (lazy only) → switch → ACK validation (`scope`, `deferred`, `confirm_required`) → confirmed/queued/unresolved. Correlate replies by stored ID, runtime ID, and connection; stale replies are dropped. Capture resume `model`/`provider` into state. Never global. Keep explicit reconnect without prompt replay.
3. **Add deterministic local fuzzy filtering.** Dependency-free case/separator-normalized multi-token subsequence scorer with stable ranking; provider+model pairs stay distinct; tests for matching/ranking/identity boundaries only.
4. **Build the composer picker.** Native popover with search, bounded results, current-pair mark, loading/no-match/error states, explicit refresh, confirmation for `confirm_required`, and busy-time "queued for next turn" labeling. Gating: no switch while another switch is pending or the outcome is ambiguous; Enter in the picker never submits; Escape restores composer focus and draft.
5. **Verify and document.** Run guardrails and focused tests; update `remote_smoke`; exercise the live Wayland picker against the remote server including empty-chat switching, busy switching, reconnect/resume, and 640×480 layout; record proof and fixture cleanup; update architecture docs.

## Acceptance criteria
- [ ] Picker is below the editor inside the composer; collapsed and expanded views show provider and model and fit the minimum 640×480 window.
- [ ] Options come from the configured remote catalog; same-named models across providers remain separate and route to the chosen provider via canonical tokens.
- [ ] Fuzzy multi-token queries match provider/model text; ranking is stable; empty query restores order; no-match is explicit; typing never sends network requests or selects a model.
- [ ] Keyboard-only open/search/navigate/select/dismiss works; hover never commits; picker Enter never sends; accessible labels present.
- [ ] An empty idle chat can switch model before its first message, and the first inference uses the chosen pair (prime+switch contract).
- [ ] Initialized idle chats switch live; busy chats queue the switch for the next turn with an explicit label, or are disabled if the probe records the gateway as unsupported.
- [ ] Definite rejection/cancel keeps the previous confirmed selection and draft; `confirm_required` uses explicit confirmation; ambiguous transport outcomes stay unresolved, never silently retried.
- [ ] Replies are scoped to the correct conversation/runtime/connection; chat switching or reconnect cannot apply stale state or replay prompts/changes.
- [ ] Reconnect/resume restores the per-chat selection from resume fields; lazy `model.options` is never used as evidence; unresolved states are labeled honestly.
- [ ] Enter-to-send, Shift+Enter newline, Stop/Stopping, clarification/approval dispatch, history paging, tool-summary rows, and per-chat drafts remain intact.
- [ ] Focused tests, formatting, strict Clippy, release build, guardrails verdict, live remote and native acceptance pass; disposable fixtures deleted with scan evidence.

## Verification
- **Contract probe first:** step 1 is go/no-go. Read global identity before/after; prove effective state via `usage.model` and resume fields, never lazy `model.options`. All arms disposable; fixtures deleted and scanned.
- **Focused permanent tests:** identity normalization (`custom:` prefix), fuzzy matching/ranking, prime/switch/deferred state transitions, stale-reply rejection, busy-state gating, scope validation of ACKs.
- **Deterministic contract:** `guardrails.json` v2 is authoritative; run `/b-guardrails-check` at coherent points. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`, release build.
- **Native Wayland acceptance:** release app against configured remote; empty-chat switch before first message; busy switch labeling; search cases; long IDs; keyboard-only; reconnect/resume; minimum-size layout; screenshots recorded.
- **Regression smoke:** composer Enter/Shift+Enter, two-turn context, Stop/Stopping, history rendering, drafts after switching chats.

## Risks
- The prime+switch contract may fail on the deployed gateway revision (it reports 0.23.0 but the exact source is unconfirmed). Stop-and-replan; fallback candidates are create-time override for new chats only or an upstream patch request.
- Busy-time deferred semantics may differ from source; A-3's probe result decides between queued labeling and busy-disabled switching.
- Long agent builds make switching feel slow; bounded waiting with typed states is the accepted tradeoff.

## Revision Log

### 2026-11-09 — Post-diagnosis solution
- Replaced the single-step empty-chat switch with a prime+switch contract after diagnosis proved the ACK-without-switch failure persists with canonical provider tokens.
- Added busy-time deferred switching per user instruction "allow switching at any time", gated by probe A-3.
- Added durable guardrails to verification; removed stale "no guardrails.json" claim.

### 2026-11-09 — Handle the gateway's whitespace parser
- Modified: Send only safely representable unquoted tokens; display unsafe catalog rows as unavailable.
- Removed: Universal shell quoting; incompatible with the observed gateway.
