---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, remote-contract, parser, blocker]
informs: [plan-model-picker.md]
---
# Disposable model-switch contract smoke

## Result: no-go

The first planned switch used shell-quoted model and provider tokens, as required by the plan. The configured remote rejected the session-scoped `config.set` with error 5001: `Unknown provider ''multi-model-gateway''`. The single quote characters were treated as part of the provider. No switch was acknowledged; no prompt was submitted. The global provider/model read from `model.options` was unchanged before and after.

The version-tagged upstream [`model_switch.py`](https://github.com/NousResearch/hermes-agent/blob/v0.23.0/hermes_cli/model_switch.py) uses `raw_args.split()` for tokenization, not shell parsing. This is consistent with the observed rejection, but does not prove the remote is running that exact source. The plan's requirement to quote arbitrary opaque IDs is incompatible with the observed remote, and whitespace-containing IDs cannot safely be represented under the source-documented parser. The plan explicitly makes a protocol mismatch a stop-and-replan condition; no application implementation was started.

The user approved disabling unrepresentable catalog IDs. A subsequent unquoted switch to `deepseek-flash` on `multi-model-gateway` received a session-scoped acknowledgment, but immediate `model.options` for the lazy runtime still reported the global `glm-5.3` default. This is not proof of the effective model. Global catalog identity remained unchanged; both empty probe sessions were closed, and deletion returned 4007 after closing. Inference, isolation with initialized agents, and reconnect/resume remain required.

An additional disposable session received the same successful acknowledgment before its first prompt. Both subsequent prompts completed, but each `message.complete.payload.usage.model` reported `glm-5.3`; initialized `model.options` also reported `glm-5.3`, not requested `deepseek-flash`. The other session and global catalog still reported the original default. Thus the server's success response is not reliable proof of a pre-first-turn switch. This directly fails the plan's contract go/no-go gate; inference and resume of a successfully changed model have not been established.

## Fixture cleanup

All eight disposable sessions were closed. Sessions with persisted turns were deleted successfully; `session.delete` returned 4007 for empty sessions after close. A complete authenticated paginated session-list scan of 1,936 rows found none of their stored IDs. No existing chats were prompted or modified.

## Resolution needed

The gateway must provide or document a session-scoped operation that actually switches an uninitialized conversation before the first prompt, with effective-model readback and resume persistence. An alternative requires a revised plan that still satisfies the user's first-message and per-session requirements. Repeat the disposable contract proof before implementing the picker. Do not equate the success acknowledgment with an effective switch.
