# Changelog

## Unreleased

### Added
- Automatic reconnect with exponential backoff. After an unintentional socket loss the client retries in 1s, doubling to a 30s cap, and drains queued commands once the gateway is ready again. Bad-password and rate-limited logins are not retried automatically.

- Attach UTF-8 text files and text-bearing PDFs to chat messages. Their extracted text is included in conversation context alongside existing image attachments. Files can be selected with **Attach** or Ctrl+U, or dragged into the composer.

- Group assistant text and tool activity in one response card. A running tool shows a short activity line with a type-specific icon and a redacted preview when the gateway sends one; on completion the line stays as a finished notation. Saved tool rows show the tool name, not arguments or output.

### Fixed

- Idle disconnects: after 30s of inbound silence the client now sends a WebSocket Ping, so healthy idle connections are no longer dropped by the 90s silence rule; only a link that stays silent despite probes is disconnected.
- Reconnect stampedes: the automatic retry backoff now carries equal jitter (each attempt lands in the second half of its scheduled delay, sampled from the clock), so clients dropped by the same gateway restart retry over a spread instead of all hitting the gateway on the same instant.
- Streaming decode cost: per-token delta frames now parse straight into borrowed fields instead of building a full JSON document tree per frame, cutting per-frame allocations on the streaming hot path to the event's own strings; every other frame keeps its single full parse with routing unchanged.
- Streaming id churn: delta events now carry shared (`Arc<str>`) runtime and message ids and the pending-tail entry survives successful sends, so a steady streaming frame allocates exactly one buffer (the delta text) instead of seven strings; the budget is pinned by a count-of-allocations regression test.
- Streaming render cost: streamed text now updates only the trailing transcript row in the UI. A batch of deltas used to rebuild and byte-compare every stored message row (~16×/second on long conversations) to reach the one row that changed; a stream-only change now splices just that row, so long-conversation streaming stays responsive instead of lagging as the transcript grows.
- Wedged-socket writes: outbound frames now ride a background writer, so a gateway that stops reading its socket can no longer stall the transport loop for up to 5s per write (a burst of commands or keepalive pings serialized 5s each). The loop only enqueues — inbound frames, UI events, and queued actions keep flowing while the dead link is detected in the background at the same 5s bound, and every in-flight request fails immediately with the disconnect instead of waiting out its full 30s RPC deadline.
- Expired gateway sessions: a REST action that the gateway answers with 401 (login cookie expired, or a gateway restart invalidated the session) now reports the rejected login session to the transport loop, which tears the live socket down once and lets the regular reconnect re-run password sign-in — the link heals itself instead of failing every list, history, search, upload, and patch until the app restarts. A rejected password still stops automatic retries exactly as before; an expired session is transient by design.
- Wedged-socket dials: the WebSocket handshake now carries its own 10s bound instead of the 30s RPC timeout, so a gateway that accepts connections but never completes the handshake fails each reconnect attempt three times faster and the backoff cycle reaches its next attempt ~20s sooner.
- Post-reconnect delay: queued commands now run before the workspace and skill catalogs are fetched, and the two catalog requests run concurrently instead of serially, so reconnecting no longer blocks user actions behind catalog round-trips (previously up to 60s worst case).
- Stuck sign-ins: pressing Reconnect during an in-flight connection attempt now aborts it and retries immediately; previously the press was ignored until the 90-second connect bound expired.
- Streaming resume latency: parked message tails now hand over on the next inbound frame as soon as the UI catches up, instead of waiting for the 5s maintenance tick; visible text resumes within one frame (typically tens of milliseconds) after a brief UI stall.
- Wedged writes: socket writes now time out in 5s instead of 30s, so a gateway that stops reading drops the link promptly and the automatic reconnect takes over rather than freezing the network loop.

- Post-ready stalls: the workspace and skill catalog fetches now run off the network loop, so a slow or stuck gateway cannot freeze frame processing, ping answering, or command handling right after connecting (previously up to 30s of transport-loop blockage on every connect).

- Reasoning stalls: thinking and reasoning stream events now use the same non-blocking park-and-flush path as message text, so a busy UI can no longer freeze the network loop during a chatty reasoning stream; parked reasoning replays with correct replace semantics and never leaks into the next turn.

- Session-read and mutation stalls: the conversation list, saved-history, session-search, and pin/rename patch requests now run off the network loop, so a slow gateway cannot freeze frame processing while you list, open, search, pin, or rename conversations — including the automatic refresh right after reconnecting (previously up to 30s of transport-loop blockage per request).

- Upload stalls: attached images now upload concurrently and off the network loop, so sending a message with attachments no longer freezes frame processing while the gateway accepts the files (previously up to 30s per image, serially). Stale uploads after a retry or reconnect are discarded by the turn guard.
- Branch stalls: forking a conversation now fetches both its earlier messages and the new branch's first page off the network loop (the seed fallback rides on a failed or empty fetch), so a slow gateway cannot freeze frame processing while the branch is prepared (previously up to 30s per history page, serially).

- Archive stalls: archiving a conversation and flushing deferred pin/rename changes after a message completes now patch off the network loop, so a slow gateway cannot freeze frame processing during archival. With this, no HTTP call runs on the network loop anymore.

- Read-only resumes: opening a conversation the gateway refuses to continue now loads its saved transcript off the network loop, so a slow gateway cannot freeze frame processing during the load.

- Resume stalls: opening a conversation no longer fetches its saved history on the network loop, so a slow gateway cannot freeze frame processing while a conversation loads — including the automatic reload after reconnecting (previously up to 30s of transport-loop blockage on the most common open path).
- Undo and compress stalls: rewinding or compacting a conversation no longer fetches the updated history on the network loop, so a slow gateway cannot freeze frame processing while the action completes (previously up to 30s of transport-loop blockage per action).

- Ghost turns after a reconnect: off-loop fetches are now stamped with the connection they ran under, and results from a torn-down connection are discarded — so a resume that raced a disconnect can no longer leave a permanently "submitted" phantom turn, send requests for a dead runtime, or replay stale approval prompts on the new connection.
- Deferred renames and pins no longer fail silently: when a conversation's saved title/pin is flushed after its first message, a refused save now re-defers the change and surfaces a curation error instead of dropping the outcome — previously the rename or pin silently reverted on the next conversation-list refresh. Concurrent patches for the same conversation are serialized, so a background flush can no longer cause a confusing "HTTP 409" on a pin or rename pressed at the same time.
- Commands issued mid-reconnect: commands pressed while a reconnect dial is in flight now follow the same rule as when disconnected — list, create, resume, search, and studio fetch wait for the new connection, while submit, steer, interrupt, and timeline actions fail immediately with "Not connected. Reconnect to try again." instead of replaying against a runtime id from the torn-down connection, which previously produced an opaque gateway failure.

### Limitations
- Up to six files can be attached per message. Each file is limited to 15 MB, and extracted document text is limited to 1 MB per file. Unsupported binary files and PDFs without extractable text are rejected; scanned PDFs require OCR first.
