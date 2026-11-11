---
date: 2026-11-11
subject: 2026-11-11.steering
topics: [steering, session-redirect, busy-turn, hermes-gateway]
informs: [plan-steering.md]
status: completed
---
# Gateway steering contract (NousResearch/hermes-agent v0.23.0)

Source: GitHub tag `v0.23.0` only, unless a line is explicitly labeled `main`.
No live server and no local Hermes install were used.

`_handle_busy_submit` on `main` is byte-identical to v0.23.0. The `prompt.submit` busy-admission loop on `main` matches the v0.23.0 loop quoted below. Dedicated `session.steer` / `session.redirect` handlers on `main` match the v0.23.0 control flow quoted below.

## Verdicts

1. **Busy-turn `prompt.submit` is policy-dependent, not one fixed outcome.** Default `display.busy_input_mode` is `interrupt`. A text-only submit then tries in-place `redirect` (same turn). If redirect is unavailable it **queues and hard-interrupts** so the follow-up runs after the live turn stops. `queued: true` or mode `queue` queues only. Mode `steer` injects into the live turn and must not hard-interrupt. Truncation while running is the real rejection: **4009 `session busy`**. Hosted-room internal submits get **4091**.
2. **Yes, dedicated RPCs exist:** `session.steer`, `session.redirect`, plus side paths `prompt.btw` and `prompt.background`. `subagent.steer` / `subagent.interrupt` are separate. There is no `interaction.respond`.
3. **The Ink TUI can type and submit while a response streams.** Enter while busy follows `display.busy_input_mode`. Canonical clients call `session.steer`, `session.redirect`, or `prompt.submit` with `queued: true`. `/steer` always calls `session.steer`. `/queue` is a local enqueue.
4. **Steer and redirect do not end the in-flight turn.** No `message.complete` at accept time. The partial stays in the same turn. A hard interrupt (default busy-mode fallback, and `session.interrupt`) ends the turn later with **`message.complete` `status: "interrupted"`**. There is no `message.error` event. `session.interrupt` is an ack: it also **drops the server FIFO** and cancels open clarify/approval requests.
5. **`prompt.submit` can land while a clarify/approval is pending.** The turn stays `running`, so the busy policy applies. Nothing requires `clarify.lock`, `approval.respond`, or `request.answer` first.
6. **Accepted steer/redirect does not start a new stream.** Same `message.start` / `turn_id`, further `message.delta`, one `message.complete` when that turn ends. A queued follow-up becomes a **new** turn (`message.start` again) only after the live turn settles and the FIFO drains.

## 1. Busy-turn `prompt.submit`

Idle submit returns `{status: "streaming"}` and starts the turn thread:

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_prompt.py#L965-L973

While `session["running"]` is true, submit does not start a second turn. Truncation is rejected. Everything else goes to `_handle_busy_submit`:

```python
# tui_gateway/methods_prompt.py (v0.23.0 lines 894-913)
while True:
    with session["history_lock"]:
        if not session.get("running"):
            break
        if internal_hosted_submit:
            return _err(rid, 4091, "hosted room member session is busy")
        busy_transport = t or session.get("transport")
    if has_truncation:
        # A rewind/edit/restore/regenerate must land as a truncation, never as a
        # steered correction or a plain follow-up queued to run after the live turn.
        return _err(rid, 4009, "session busy")
    busy_response = _handle_busy_submit(
        rid, sid, session, text, busy_transport,
        queued=bool(params.get("queued")), turn_author=turn_author,
        display_kind=display_kind)
    if busy_response is not None:
        return busy_response
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_prompt.py#L889-L913

OpenRPC summary: "Send a user turn to a live session; busy sessions queue / steer / redirect instead of refusing." `PromptSubmitStatus` is `streaming | queued | steered | redirected`. Param `queued` forces queue mode.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/apps/shared/src/gateway-contract.openrpc.json

### Policy (`display.busy_input_mode`)

Default when unset or unknown: **`interrupt`**.

```python
def _load_busy_input_mode() -> str:
    raw = str(_display_cfg().get("busy_input_mode", "") or "").strip().lower()
    return raw if raw in {"queue", "steer", "interrupt"} else "interrupt"
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/server.py#L249-L251

```python
def _handle_busy_submit(...):
    """interrupt (default) -> redirect, falling back to hard interrupt + queue;
    queue -> queue only; steer -> inject after the current atomic action.
    queued=True forces queue mode: a run-after message must NEVER become a live correction."""
    mode = "queue" if queued else _load_busy_input_mode()
    if mode in ("steer", "interrupt") and _session_compression_in_flight(session):
        mode = "queue"
    # text-only: steer() -> {status: steered}; redirect() -> {status: redirected}
    # else enqueue + persist user row at accept
    # steer must NEVER escalate to a hard interrupt
    if mode == "interrupt" and not image_paths:
        _interrupt_busy_session(sid, session, agent)
    return _ok(rid, {"status": "queued"})
```

Full function: https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_auto_continue.py#L400-L454

| Mode | What `prompt.submit` does | RPC result |
| --- | --- | --- |
| `interrupt` (default), text-only, agent supports redirect | Inject via `agent.redirect` (same turn). No queue. | `{status: "redirected"}` |
| `interrupt`, redirect unavailable, no images | FIFO enqueue, persist user row at accept, then hard-interrupt the live turn | `{status: "queued"}` |
| `interrupt` + images | Queue only. Do not cancel. | `{status: "queued"}` |
| `steer`, `agent.steer` accepts | Inject. No hard interrupt. | `{status: "steered"}` |
| `steer`, steer unavailable / rejected | FIFO only. Must not hard-interrupt. | `{status: "queued"}` |
| `queue`, or `queued: true` | FIFO only. | `{status: "queued"}` |
| compression lock held | steer/interrupt demoted to queue | `{status: "queued"}` |
| truncation params while running | Rejected. Client should interrupt, wait, resubmit. | **4009** `session busy` |
| hosted-room internal submit while running | Rejected | **4091** |
| turn ended between the busy check and the handler | `None`; caller retries the idle path | falls through to `streaming` |

Queue mechanism (not the config.set stash): `session["queued_prompt"]` head plus `session["queued_prompts"]` tail, written by `_enqueue_prompt`. Accept-time durable user row, then re-placed at the transcript end on drain so repair does not glue two user turns.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_auto_continue.py#L147-L176
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_auto_continue.py#L292-L348
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_auto_continue.py#L457-L509

Other busy rejections, not the normal composer case:

- **4009** `subagent still running — wait for it to finish` — lazy watch session whose child is still running. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_prompt.py#L791-L795
- **4090** — session owned by another gateway process and the forwarded prompt was not confirmed, or cannot join that queue. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_prompt.py#L729-L764


## 2. Dedicated steering RPCs

| Method | Contract doc |
| --- | --- |
| `session.steer` | Inject text into the next tool result without interrupting the turn. |
| `session.redirect` | Redirect the active turn (queued for the next turn while the agent is still building). |
| `session.interrupt` | Stop the running turn (and streaming TTS); retires the crash-recovery marker. |
| `prompt.btw` | Side question over a snapshot; answer is `btw.complete`. History untouched. |
| `prompt.background` | Fresh agent; answer is `background.complete`. |
| `subagent.steer` / `subagent.interrupt` | Child agents, not the parent composer. |

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/contracts/sessions.py#L620-L642
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_prompt.py#L1181-L1224

No `interaction.respond`. Clarify is `clarify.lock`. Approvals are `approval.respond`. A mirrored server-to-client request is `request.answer`.

`session.steer` / `session.redirect` params: `session_id`, optional `profile`, required `text`. Result `status`: `queued | redirected | rejected`, plus `text`. Extra `execution_generation` is **not** in this schema (`additionalProperties: false`). The TUI still sends it on the canonical path.

```python
# accepted steer status string is "queued", not "steered"
_correction_method("session.steer", "steer", "queued",
                   lambda agent: hasattr(agent, "steer"),
                   "agent does not support steer")
_correction_method("session.redirect", "redirect", "redirected",
                   lambda agent: getattr(agent, "_supports_active_turn_redirect", False) is True
                   and hasattr(agent, "redirect"),
                   "agent does not support active-turn redirect")
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_session_interrupt.py#L133-L176

- Empty text: **4002**.
- Agent missing the method: **4010**.
- `session.steer` while not `running`: `{status: "rejected"}` so the client queues a normal next prompt.
- `session.redirect` while `running` but agent still building (`agent is None`): enqueue, `{status: "queued"}`.
- Compression in flight: enqueue, `{status: "queued"}`.
- Accepted `steer()`: `{status: "queued", text}` — this "queued" means the steer buffer, not the FIFO.
- Accepted `redirect()`: `{status: "redirected", text}`.
- `agent.steer` / `redirect` returns false: `{status: "rejected", text}`.

`prompt.submit` steer success is `{status: "steered"}`. `session.steer` success is `{status: "queued"}`. Same word, different meaning.

## 3. Upstream TUI: type and submit while streaming

Composer is not locked while busy. Typing during a stream calls `turnController.boostStreamingForTyping()`.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/useSubmission.ts#L109-L111

Enter while `getUiState().busy` calls `handleBusyInput` instead of a fresh idle submit:

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/useSubmission.ts#L273-L329
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/useSubmission.ts#L480-L482

Comments there (v0.23.0):

- `queue`: local queue; drain when busy goes false. Canonical instead admits now with `queued: true`.
- `steer`: `session.steer`; if rejected, queue as the next prompt.
- `interrupt` (default): submit immediately; backend redirects the active model request, or steers after a tool, with interrupt+queue as the compatibility path.

| Gateway | `busyInputMode` | RPC | Params |
| --- | --- | --- | --- |
| canonical | `steer` | `session.steer` | `session_id`, `text`, and `execution_generation` when the client has one |
| canonical | `interrupt` (default) | `session.redirect` | same shape |
| canonical | `queue` | `prompt.submit` | `session_id`, `text`, `submission_id`, **`queued: true`** |
| non-canonical | `queue` | none yet | local `enqueue`; later drain is a normal `prompt.submit` |
| non-canonical | `steer` / `interrupt` | `prompt.submit` | `session_id`, `text`, no `queued` flag — server applies `display.busy_input_mode` |

Param assembly: https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/submissionCore.ts#L136-L154
Call: https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/submissionCore.ts#L413-L425

Images on a steer/redirect control are refused client-side and never sent. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/submissionCore.ts#L403-L410

Slash commands:

- `/steer` (alias `/s`): `session.steer` `{session_id, text, execution_generation?}` if busy; otherwise local enqueue. Rejected steer is enqueued locally. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/slash/commands/core.ts#L866-L906
- `/queue` (alias `/q`): local enqueue only. Does not call the gateway. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/slash/commands/core.ts#L848-L863

Double-Esc / Ctrl+C while busy and the composer is empty calls `session.interrupt`, not a steer. An approval/clarify overlay owns Esc/Ctrl+C until answered. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/ui-tui/src/app/useInputHandlers.ts#L137-L159

## 4. Interrupt / steer interplay, partials, history

### Steer does not end the turn

```python
def steer(self, text: str, *, ticket=None) -> bool:
    """Queue user text for delivery as its own user row after the current tool batch
    finishes (no interrupt); multiple calls concatenate with newlines."""
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/interrupt_control.py#L310-L321

Delivery is a new `role: "user"` row with `display_kind: "steer"`, after the newest tool result. Not a new `prompt.submit` turn.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/turn_iteration_prep.py#L175-L181
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/prompt_builder.py#L586-L592
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/agent_runtime_helpers.py#L3668-L3702

If the turn ends before that drain, leftover steer is `result["pending_steer"]` and the gateway enqueues it as the next prompt.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/turn_finalizer.py#L847-L849
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L441-L451

No `message.complete` at steer-accept time. The live `message.delta` stream continues. One `message.complete` when `run_conversation` returns.

### Redirect continues the same turn

```python
def redirect(self, text: str) -> bool:
    """during a model request only that request is cancelled (completed messages kept,
    partial reasoning becomes assistant context, the correction is appended as a real
    user message, the loop retries); during tool execution it degrades to steer()."""
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/interrupt_control.py#L323-L384

During tools: becomes `steer()` and asks tool workers to yield. Does not kill the tool.
During a model request: sets `_pending_redirect`, aborts that request only.
If the model request already finished: returns false; the RPC surface queues a new turn.

On the next iteration the loop applies the correction inside the same turn:

```python
_redirect_text = agent._drain_pending_redirect()
if _redirect_text:
    _apply_active_turn_redirect(agent, messages, _redirect_text)
    agent._persist_session(messages, conversation_history)
if agent._interrupt_requested:
    interrupted = True
    return _verdict("break")
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/turn_iteration_prep.py#L370-L388

Partial preservation (`_apply_active_turn_redirect`):

- Visible streamed text (think-blocks stripped) is kept as an assistant row.
- Empty/runaway partial becomes a hidden interrupt placeholder, not raw chain-of-thought.
- User sees their own words as a user row. The provider sees a scaffold whose first line is `[This response was interrupted by a user correction.]`.
- `_stream_needs_break = True` so the next text delta is prefixed with a blank line. That is not a new `message.start`.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/conversation_loop.py#L73-L74
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/conversation_loop.py#L324-L363

A successful redirect does **not** emit `message.complete` for the cancelled request. The same turn's stream continues. `message.complete` fires once, when the continued turn ends.

### Hard interrupt ends the turn

`session.interrupt` returns immediately. It does not wait for `message.complete`.

```python
_interrupt_session_turn(sid, session)
return _ok(rid, {"status": "interrupted"})
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/methods_session_interrupt.py#L70-L105

The turn thread later emits `message.complete`, not a separate error event:

```python
def _result_status(result: dict) -> str:
    return (
        "interrupted" if result.get("interrupted")
        else "error" if result.get("error") else "complete")
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L313-L316
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L1188-L1193

If the only assistant text is the sentinel `Operation interrupted: waiting for model response`, `message.complete.text` is cleared to `""`. Already-streamed deltas are not unsent. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L330-L337

History: an interrupted tail that ends on a tool row gets a closing assistant row. Empty/placeholder text is `display_kind: "hidden"`. A real partial stays visible. `stopped=True` sets `display_metadata.interrupted = true`.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/message_sanitization.py#L288-L319
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/agent/turn_finalizer.py#L240-L248

`persisted_turn.complete` is false across redirects and partial writes. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/contracts/events.py#L263-L276

### `session.interrupt` drops a queued follow-up

```python
session["_turn_cancel_requested"] = True
session["queued_prompt"] = None
session.pop("queued_prompts", None)
session["_queued_prompt_generation"] = int(session.get("_queued_prompt_generation", 0)) + 1
```

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_lifecycle.py#L771-L797

It also hard-interrupts the agent, cancels open server-to-client requests (`request.cancel`, reason `interrupted`), deny-resolves pending approvals, and retires the crash-recovery marker so resume will not auto-continue the stopped turn.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/server.py#L1105-L1109
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_lifecycle.py#L835-L844

No generation argument on `session.interrupt` (`session_id`, optional `profile`, optional `expected_hosted_task_id` only). The ack is not proof the turn has finished. `message.complete` with `status: "interrupted"` is the terminal frame.

Default busy mode's "interrupt then queue" uses `_interrupt_busy_session` (agent `interrupt()`, not `session.interrupt`). That path **keeps** the FIFO it just wrote. `session.interrupt` deletes it.

`interrupt()` clears `_pending_redirect`. It does not, in the v0.23.0 body, clear `_pending_steer`. `clear_interrupt(hard_cancel=True)` does. See Confidence.

## 5. Clarification / approval pending

No `AwaitingInput` symbol and no `interaction.respond` method in the v0.23.0 sources fetched for this note.

While a clarify or approval is open, the turn thread is still inside `run_conversation`, so `session["running"]` stays true. `prompt.submit` therefore hits section 1, not a "respond first" error. A text follow-up can be redirected, steered, or queued under `busy_input_mode` without `clarify.lock` / `approval.respond` / `request.answer`.

`session.interrupt` is the path that cancels those prompts (`_clear_pending`) and deny-resolves approvals. The TUI will not send that interrupt from Esc/Ctrl+C while the overlay is up.

`prompt.btw` is the explicit side-question RPC. It snapshots history and does not touch alternation. Answer event: `btw.complete`. It is not what composer Enter calls.

## 6. Events a client sees

One prompt turn stamps one stream identity, then emits `message.start` once (unless muted):

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L1155-L1160

Deltas are `message.delta` `{text, rendered?}`. A null `text` is a segment boundary, not a new turn. `message.interim` seals commentary beside tool calls. Same `turn_id` means the same turn.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/contracts/events.py#L135-L195
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/prompt_turn.py#L768-L792

`message.complete` is once per turn: `{text, usage, status}` where `status` is `complete | interrupted | error`. Optional `partial: true` on errors that kept answer text. There is no `message.error` event. Session-level failures use `error`.

https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/contracts/events.py#L198-L305
https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/contracts/events.py#L120

| Accept result | New `message.start`? | Stream | Terminal frame |
| --- | --- | --- | --- |
| `prompt.submit` `{status:"steered"}` or accepted `session.steer` `{status:"queued"}` | No | Same `turn_id`. Later user row `display_kind: "steer"`. More `message.delta` after the next tool batch. | One `message.complete` for the original turn |
| `redirected` | No | Same `turn_id`. Next deltas continue. History gains assistant checkpoint + user correction. | One `message.complete` when the continued turn ends |
| FIFO `{status:"queued"}` | Not at accept | Live turn keeps streaming | Live turn's `message.complete` (`interrupted` if a hard interrupt was also fired, else `complete`). Drain then calls `_run_prompt_submit`, which emits a **new** `message.start`. https://github.com/NousResearch/hermes-agent/blob/v0.23.0/tui_gateway/session_auto_continue.py#L505-L509 |
| `session.interrupt` `{status:"interrupted"}` | No | Deltas stop when the agent honors the interrupt | Later `message.complete` `status: "interrupted"`. FIFO already cleared, so no follow-up `message.start` from that queue. |

`session.steer` cannot be distinguished from a FIFO fallback by `status` alone: both return `queued`. `prompt.submit` can: steer success is `steered`, redirect success is `redirected`, FIFO is `queued`.

## Confidence / unverified

- **v0.23.0 vs `main`:** `_handle_busy_submit` identical. Busy-admission loop identical in the quoted region. `session.steer` / `session.redirect` control flow matches. Not a full-tree diff.
- **`interrupt()` vs `_pending_steer`:** `_handle_busy_submit` says a hard interrupt drops `AIAgent._pending_steer`. v0.23.0 `interrupt()` clears `_pending_redirect` only. `clear_interrupt(hard_cancel=True)` wipes the steer buffer. The turn finalizer's `clear_interrupt()` at `turn_finalizer.py:854` does not pass `hard_cancel`. Whether a hard stop still drops an undrained steer was not fully traced. Treat "hard interrupt destroys earlier steers" as a comment claim, not a verified line in `interrupt()`.
- **`execution_generation` on `session.steer`:** the TUI sends it on the canonical gateway. OpenRPC `SessionCorrectionParams` does not list it and forbids extra properties. Not verified whether the v0.23.0 dispatcher rejects that extra field. Do not depend on it until checked against the deployed gateway.
- **Clarify + redirect timing:** no line says "refuse `prompt.submit` while a clarify card is open". Admission is the busy policy because `running` stays true. Not verified whether a clarify wait sets `_executing_tools` (redirect degrades to steer) or `_model_request_active` (redirect aborts the request).
- **Partial text in SQLite after a pure model-request interrupt:** streamed deltas are already on the wire. The durable close is `close_interrupted_tool_sequence` when the tail is a tool row. A stop during the first tokens, before any tool row, was not line-traced to a specific `messages` append. `message.complete.text` may be `""` even if the client already rendered deltas.
- **4090** is the cross-process owner path, not the normal single-gateway busy path. Not exercised.
- No live v0.23.0 process was queried. Tag `v0.23.0` is the source of every quote above.
