---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [remote-contract, model-switch, prime]
informs: [plan-model-picker.md, phase-1-contract-proof.md]
---
# Prime-then-switch contract proof

On the configured remote gateway, two disposable chats were created. A session-scoped provider-less prime using the default `glm-5.3` retained `glm-5.3` in the first completion's `usage.model`. A second chat received the same prime, then `deepseek-flash --provider custom:multi-model-gateway --session`; its first completion's `usage.model` was `deepseek-flash`. All three `config.set` replies reported session scope with no confirmation required. The global model and provider read before and after were unchanged.

Both runtime sessions were closed and both stored sessions deleted. An authenticated full paginated scan of 1,965 sessions found neither fixture. No existing user conversations were modified.

## Additional remote observations

In a second pair of disposable chats, an initialized session switched to `deepseek-flash`, and cold-resume `info` and subsequent inference agreed on that pair. An isolated chat used the advertised default `glm-5.3`; the global catalog identity remained unchanged. The initial inference in the other newly created chat unexpectedly reported `grok-4.7` although the global catalog reported `glm-5.3`. The cause is unverified; do not infer every unprimed new chat uses the advertised default.

A session-scoped switch sent immediately after the submit acknowledgement returned `deferred: true`, but the just-submitted turn and the next turn both reported the target `glm-5.3`. This timing does not establish a mid-stream switch; it disproves treating every deferred response received while submitting as necessarily applying only to a subsequent turn. Busy UI must not claim a next-turn guarantee from this observation.

Both sessions were closed and deleted. A full paginated scan of 1,965 sessions found neither stored ID.

## Confirmed mid-stream behavior

Another disposable chat was created with `deepseek-flash` explicitly. After a `message.delta` and before any terminal event, a switch to `glm-5.3` returned `deferred: true`. The running turn's `usage.model` remained `deepseek-flash`; the next turn used `glm-5.3`. Global identity was unchanged. The session was closed and deleted; a full 1,965-row scan confirmed absence. This confirms next-turn behavior after streaming begins, not during the earlier submit-to-start race.

## Default identity diagnostic and remaining gate

A separate unprimed disposable chat reported `glm-5.3` in its create metadata, first inference, initialized `model.options`, and cold-resume metadata. It was closed and deleted; the full 1,965-row scan found no fixture. This confirms agreement in that case but does not explain or invalidate the earlier `grok-4.7` observation. The plan's promise that the pre-first-turn prime preserves the actual default is not established across that contradictory observation. Determine whether the gateway routed or fell back to a different inference model before displaying an unverified pair as confirmed.

Native GUI acceptance is also blocked: `orca-ide skills get computer-use --json` returned `command not found`. Wayland and a monitor are present, but their presence alone does not establish that the prescribed native computer-control tool works.
