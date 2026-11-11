---
status: completed
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [hermes, model-picker, gateway, session-scope]
---
# Hermes model contract evidence

## Primary-source findings
Research used version-tagged `v0.23.0` Hermes source; live server behavior takes precedence.

- `model.options` RPC accepts `session_id` (runtime ID), `explicit_only`, `include_unconfigured`, `refresh`, `profile`. Use `{explicit_only:true, include_unconfigured:false, refresh:false}` for the configured chat-picker catalog.
- Top-level provider/model comes from the actual live agent only when a valid runtime ID resolves an initialized agent. Otherwise it falls back to profile defaults. Stored IDs and nonexistent runtime IDs silently miss live state.
- `session.create` supports optional provider/model overrides. `session.resume` can report restored effective provider/model under `info`; lazy/uninitialized sessions may not provide them. `prompt.submit` does not accept model/provider.
- Existing-session model change is `config.set {session_id:<runtime>, key:"model", value:"<model> --provider <provider> --session", confirm_expensive_model:false}`.
- Successful source-defined response includes `{key:"model", value:<resolved model>, warning, confirm_required, confirm_message, scope:"session"}`. Busy switches can return `deferred:true`; native picker will forbid changes while busy rather than offer queued changes.
- `--session` is mandatory: omitting it can write global defaults depending on Hermes persistence settings. Never use `POST /api/model/set` for the composer: that route changes global config.
- If `confirm_required:true`, use explicit native confirmation and retry the same target only after approval with `confirm_expensive_model:true`; cancellation retains the old model and draft.
- Session-scoped overrides are persisted by Hermes runtime metadata and restored during resume according to the version-tagged implementation. No parallel client database is needed.
- Provider identity is `(canonical provider, opaque model ID)`. Custom provider row slugs may omit `custom:` while active provider strings include it. Prefer advertised aliases when present; otherwise normalize only the documented user-defined prefix for comparison, and send an unambiguous canonical provider token. Do not silently retry a different provider or model.
- Provider model strings are curated/discovered server data. No third-party catalog, arbitrary IDs, onboarding, pricing controls or model capability settings belong in this feature.

## Live verification and important disagreement
Read-only/guard-only probes against the configured remote on 2026-11-09:

| Call | Observed result |
| --- | --- |
| Password login / WebSocket ticket | HTTP 200 / 200; `gateway.ready` received |
| `model.options {}` | Successful catalog: 18 provider records, 485 provider-model rows |
| `model.options {explicit_only:true,include_unconfigured:false,refresh:false}` | Successful configured catalog: 16 provider records, 413 provider-model rows |
| `model.options` with deliberately nonexistent runtime ID | Same reported default selection; must not treat fallback as per-chat evidence |
| `session.info` with deliberately nonexistent runtime ID | `-32601` (unknown method); **do not depend on this upstream helper** |
| `config.set` with deliberately nonexistent runtime ID, explicit provider/model and `--session` | `4001` (session not found); session guard exists and prevented a change |

No real session was created, resumed or changed; no prompt was submitted; no global settings were changed. Socket closed after the probes. No credentials, cookies, tickets or host were logged.

Successful switching, expensive-model confirmation, actual resume persistence and first-prompt initialization are implementation acceptance scenarios, not verified feature behavior. A disposable remote contract smoke is the first implementation step; if it cannot establish truthful effective state, stop and resolve the observed protocol difference instead of using defaults as fake per-chat state.

## Primary sources
- [Catalog and custom aliases](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/inventory.py)
- [RPC model.options and picker context](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_complete.py)
- [Structured config.set model switching](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_config_set.py)
- [Runtime model switching](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/model_switch.py)
- [Provider routing and persist behavior](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/model_switch.py)
- [Create/resume session behavior](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_session.py)
- [Model selection guard](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_session_model_guard.py)
- [Global HTTP model route; intentionally excluded](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/web_routers/models.py)
- [Upstream schema, reference only](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/apps/shared/src/gateway-contract.openrpc.json)

Source types: primary source repository and live remote API. Access date: 2026-11-09. Research handoff: `HermesModelContracts`.
