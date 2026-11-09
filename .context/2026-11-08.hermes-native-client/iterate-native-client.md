---
status: completed
date: 2026-11-08
updated: 2026-11-09
completed: 2026-11-09
subject: 2026-11-08.hermes-native-client
topics: [review, iteration]
addresses: plan-native-client.md
from_review: b-review
---
# Native client integration iteration

## Source

Engineering-standards review of the implemented native UI and renderer, plus mainline integration inspection against the implementation plan. Every defect below was corrected in source and covered by parent-exercised gates or native proof on 2026-11-09. No acceptance item in this list remains open.

## In-plan defects

1. Markdown table headers are lost because TableHead is not flushed before leaving header mode. Flush it and retain the existing table boundary regression. **Verified.** Native screenshots show normal-sized table headers and cells.
2. Tight list items lose inline wrappers, links, and images because implicit paragraph inlines are flattened. Preserve wrappers inside the item and add a consumer-visible regression. **Verified.** Nested lists and safe versus inert links rendered in the corrected rich screenshot.
3. Outstanding history requests are tracked only by a button's sensitivity. Switching sessions re-enables duplicate page requests. Track loading per conversation and reject duplicate or stale page offsets; add a state-boundary regression. **Verified.** Native paging loaded all 300 rows without a duplicate-page failure, and the older control disappeared at the start.
4. Nesting-limit fallback reorders text around an active paragraph. Preserve textual order through overflow and test that boundary. **Verified** by the retained regression and the rendered rich transcript.
5. Integration compiler failures: async constructor called synchronously; GtkLabel needs Pango WrapMode. Correct types and constructor. **Verified.** Final fmt, 25 tests, strict Clippy, and release build passed.
6. Transport errors can claim a prompt was not sent after a partial write, and some failure paths lack a session ID. Preserve uncertainty, stop running UI state, bound socket writes, handshake, and close, and surface worker initialization failure. **Verified** in source review and the passing remote smoke. No false "not sent" claim remains in the exercised paths.
7. Source text was silently truncated at 200,000 characters. Remove silent truncation from deltas, completions, and saved history so copy retains the original transcript. **Verified.** Copy-source retained exactly 429 characters and copy-code exactly 48. The 300-row fixture retained 108,138 content characters.
8. Canonical batch clarifications use `qid`, independent answers, optional multi-select, and nullable skipped values. Replace the single shared answer field; preserve locked answers on replay. **Verified.** The submitted native card recorded q0 `Blue`, q1 `[WebSocket,HTTP]`, q2 `live test`, and q3 skipped/null.
9. Late submit or interrupt acknowledgments and timeouts can reopen or fail a later turn. Retire accepted and terminal jobs and correlate remaining outcomes with turn generations. **Verified** by the corrected quartz probe and native Stop, which stayed busy until the real terminal event.
10. `request.cancel` resolves only the matching interaction, not the entire turn. Keep the turn running until its actual terminal event. **Verified.** Cancellation of one request does not finish the turn.
11. Preserve the delete retry flag through close so a repeated active-session refusal cannot retry forever. **Verified** in the delete-retry regression. Owned fixtures were deleted once and confirmed absent.
12. Stop draining queued commands if a send loses readiness; otherwise pop and requeue loops indefinitely. **Verified** by the bounded write-readiness correction.
13. Transient resume timeouts and teardown must not latch a loaded read-only state. **Verified.** The unreachable profile showed Disconnected, distinct from authentication failure and from the Kanban 4132 read-only refusal. Neither failure profile created a chat.
14. Server message pages are already chronological, including `order=latest`. Reversing them is incorrect. Use absolute oldest-first pagination before a fixed stored-row index so optimistic display rows and new appended server rows do not shift the cursor. **Verified** by the live order probe and native 300-row boundaries 251, 201, 151, 101, 51, 1.
15. Valid unrelated `cron.changed` notifications are not chat failures. Unsupported server requests still receive their explicit protocol error. **Verified.** The broadcast no longer false-fails an otherwise connected chat.
16. Rejected interrupt and control operations must preserve busy state. A real gateway `error` notification must finish the backend turn, not only the native status. **Verified.** Rejected interrupt stays busy; a real gateway error retires the turn and its jobs.
17. Diagnostic URL redaction must scan UTF-8 slices linearly rather than repeatedly copying the remaining suffix. **Verified** by the corrected scanner and review.
18. Wrapping native table cells caused a one-character width allocation and giant minimum height. Use natural non-wrapping cells within horizontal overflow, without truncation. **Verified** on the corrected rich screenshot.
19. GTK single-click activation also selects rows on pointer enter. Disable hover selection, suppress notifications across model splice and selection sync, and adopt intentional click or keyboard changes without focus theft. **Verified.** Hover did not select history. New stayed selected during a stationary-pointer splice. Double-click selected C; Down selected B without editor focus theft; Enter focused the editor.
20. The actual stop and next-turn probe confirmed that an interrupt ACK is not terminal. Keep a typed Stopping state busy until the old `message.complete` or error, then permit the next generation. Late text or ACK must not reopen it. **Verified.** Corrected probe: `interrupt=confirmed quartz=true`. Native Stop showed Stopping and Stop sensitive=0 before terminal Send sensitive=1, then the preserved draft completed as `quartz`.
21. A double-click can claim the GTK release before the bubble pointer-intent reset. Retire pointer intent through capture and deferred reset so keyboard selection cannot inherit it. **Verified** by the double-click then Down selection without focus theft.
22. Approval or clarification arriving after stop acknowledgment must be resolved without reopening Stopping or displaying actionable controls. The backend cancels the late request; native state refuses its adoption. Queued replies are included. **Verified.** Late requests and answers use existing RPC `-32000` cancelled and are never granted after Stop.
23. Win32-style GAction activation and the GTK `win` action passed an unboxed third field. `Variant::from` on the answer tuple produced `(sss)`, `(ssb)`, or `(ssa{sms})`. The typed action expects `(ssv)`. Box the third field with `Tuple::to_variant`. **Verified** by a throwaway real GLib roundtrip of all three shapes and by the actual native four-question submission. Reviewers did not independently review this one-line fix. The throwaway example was removed; no permanent wiring test was added.

## Verification

Final permanent tree: format check, 25 tests, strict Clippy, and release build passed. `--help` and `--version` ran and reported 0.1.0. Real two-chat remote smoke and the corrected stop, next-turn, and reconnect probe passed, and their disposables were deleted. Native Wayland proof covered context, cold resume, drafts, reload, streaming navigation, hover and keyboard selection, Stop, rich layout, PRIMARY selection, source and code copy buttons, 300-row paging, submitted clarification, and distinct authentication and unreachable profiles. A live approval card was not emitted and is not claimed. Ctrl+C prose clipboard copy did not pass and is not claimed. Same-process RSS for pid 402166 is recorded in `verification-implementation.md`. All seven long-lived fixtures are deleted per `cleanup-verification.json`.
