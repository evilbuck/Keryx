---
status: completed
date: 2026-11-09
subject: 2026-11-08.hermes-native-client
---
# Implementation work

## Scope and sequence
Implement all six slices in plan-native-client.md, then review, fix, document, save and commit. Existing memories supplied in the session are reused; the remote gateway in `.env` is the sole protocol/runtime target. Cargo generates the project. The directory initially had no Git repository; Cargo initializes it for the requested commit closeout.

## Selected course
Retain the settled Rust + GTK4/libadwaita UI, reqwest cookie jar, tokio-tungstenite JSON-RPC transport and pulldown-cmark native rendering. No browser, local agent, local Hermes compatibility target, or second conversation database. Use native GTK widget recycling and server pagination; retain explicitly loaded history without silent truncation. Keep per-session drafts and streaming state independent of selection.

## Ownership and interfaces
- Integration owner: Cargo manifest, `src/model.rs`, `src/main.rs`, `src/ui.rs`, resources, native verification, docs and workflow evidence.
- Backend slice: `src/backend.rs`, with normalized typed commands/events; remote live schema evidence and a disposable backend verification example.
- Renderer slice: `src/markdown.rs`, selectable native blocks and security boundary tests.
- Network workers never touch GTK. UI drains typed events on the GTK main context and coalesces active-row refreshes. Runtime IDs remain separate from durable IDs. Ambiguous submissions are never replayed.

## Risks and validation
- Remote agent access crosses a privileged trust boundary: authenticate only to the configured origin, reject public plaintext transport, verify HTTPS certificates, never log credentials or tickets. Validate rejection and sanitized error behavior.
- Verification prompts/deletes must target only tracked disposable ids. An early focus-driver preview opened an unverified historical row without prompting or deleting it; record that boundary honestly and never try to undo it with an unverified close/delete.
- Disconnect can leave submission outcome unknown: show interrupted/unknown status, reconnect and resume server history, never replay. Validate a disconnect during a live turn.
- Markdown is untrusted: escape Pango text, allow only HTTP/HTTPS/mailto links, render HTML literally, and preserve source for copying. Validate unsafe-link and literal-markup cases.
- Live GTK/Wayland proof is mandatory; use AT-SPI, keyboard input and screenshots on the actual application. Measure release-process RSS in idle, streaming and long-history scenarios.

## Check contract
No guardrails.json or existing source/check conventions were present. Integration used Buck's detected Rust ephemeral contract, plus the remote and native scenarios. No coverage or complexity baseline was invented. A visible warning and `/b-init-guardrails` offer remain required; do not invent a durable baseline.

## Status
Source implementation and acceptance are complete. All six plan slices are implemented and verified; the linked session memory records the portable checkpoint, and Git records commit identity.

Proof is linked, not recopied: [verification-implementation.md](verification-implementation.md) and [cleanup-verification.json](cleanup-verification.json). Final fmt, 25 tests, strict Clippy, and the post-dispatch release build passed; `--help` and `--version` ran (0.1.0). No production-source change followed that build. Real remote smoke and the corrected interrupt probe passed. Native Wayland proof covered context, cold resume, read-only refusal, drafts, resize, per-block selection and copy buttons, rich Markdown, 300-row history, clarification submission, stop/next turn, and distinct auth versus unreachable failures.

Limits: independent reviews did not re-review the final one-line GVariant fix (throwaway GLib roundtrip plus actual native four-question submission proved it; throwaway examples removed). Live approval card unobserved under server policy. Ctrl+C prose clipboard not claimed. One unverified historical row was previewed with no prompt or delete. RSS snapshots are not peaks, PSS, GPU, or a fixed budget. SQL memory is not configured; portable file memory is used.
