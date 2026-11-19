# Changelog

## Unreleased

### Added
- Automatic reconnect with exponential backoff. After an unintentional socket loss the client retries in 1s, doubling to a 30s cap, and drains queued commands once the gateway is ready again. Bad-password and rate-limited logins are not retried automatically.

- Attach UTF-8 text files and text-bearing PDFs to chat messages. Their extracted text is included in conversation context alongside existing image attachments. Files can be selected with **Attach** or Ctrl+U, or dragged into the composer.

- Group assistant text and tool activity in one response card. A running tool shows a short activity line with a type-specific icon and a redacted preview when the gateway sends one; on completion the line stays as a finished notation. Saved tool rows show the tool name, not arguments or output.

### Fixed

- Idle disconnects: after 30s of inbound silence the client now sends a WebSocket Ping, so healthy idle connections are no longer dropped by the 90s silence rule; only a link that stays silent despite probes is disconnected.
- Post-reconnect delay: queued commands now run before the workspace and skill catalogs are fetched, and the two catalog requests run concurrently instead of serially, so reconnecting no longer blocks user actions behind catalog round-trips (previously up to 60s worst case).
- Stuck sign-ins: pressing Reconnect during an in-flight connection attempt now aborts it and retries immediately; previously the press was ignored until the 90-second connect bound expired.
- Streaming resume latency: parked message tails now hand over on the next inbound frame as soon as the UI catches up, instead of waiting for the 5s maintenance tick; visible text resumes within one frame (typically tens of milliseconds) after a brief UI stall.
- Wedged writes: socket writes now time out in 5s instead of 30s, so a gateway that stops reading drops the link promptly and the automatic reconnect takes over rather than freezing the network loop.

- Post-ready stalls: the workspace and skill catalog fetches now run off the network loop, so a slow or stuck gateway cannot freeze frame processing, ping answering, or command handling right after connecting (previously up to 30s of transport-loop blockage on every connect).

- Reasoning stalls: thinking and reasoning stream events now use the same non-blocking park-and-flush path as message text, so a busy UI can no longer freeze the network loop during a chatty reasoning stream; parked reasoning replays with correct replace semantics and never leaks into the next turn.

- Session-read and mutation stalls: the conversation list, saved-history, session-search, and pin/rename patch requests now run off the network loop, so a slow gateway cannot freeze frame processing while you list, open, search, pin, or rename conversations — including the automatic refresh right after reconnecting (previously up to 30s of transport-loop blockage per request).

- Upload stalls: attached images now upload concurrently and off the network loop, so sending a message with attachments no longer freezes frame processing while the gateway accepts the files (previously up to 30s per image, serially). Stale uploads after a retry or reconnect are discarded by the turn guard.

### Limitations
- Up to six files can be attached per message. Each file is limited to 15 MB, and extracted document text is limited to 1 MB per file. Unsupported binary files and PDFs without extractable text are rejected; scanned PDFs require OCR first.
