---
status: completed
date: 2026-11-08
updated: 2026-11-09
completed: 2026-11-09
subject: 2026-11-08.hermes-native-client
scope: [rust-client, native-wayland-ui, remote-gateway, memory]
---
# Implementation verification

Verified on 2026-11-09. Application source and acceptance are complete; the [session checkpoint](../memory/hermes-native-implementation-2026-11-09.md) records the work, and Git records commit identity. The detected Rust check contract is ephemeral: no `guardrails.json`, coverage, or complexity baseline exists. `/b-init-guardrails` is available to establish a durable contract.

Independent backend and native review findings were corrected. Narrow independent re-reviews returned no findings. A later one-line GVariant boxing fix was proven by a throwaway real GLib roundtrip and an actual native four-question submission. Reviewers did not independently review that final line. Throwaway variant and edge examples were removed; no permanent wiring test was added. No production-source change followed the final release build.

## Current evidence

| Deliverable | Current evidence |
|---|---|
| Canonical Rust project | Cargo-generated Rust/GTK4/libadwaita client. No Electron, Tauri, webview, local agent, local runtime, or local server install. Only the configured remote `.env` gateway was used. |
| Final gates and CLI | After both throwaway examples were removed: `cargo fmt --all -- --check` passed; `cargo test` passed 25 tests; `cargo clippy --all-targets -- -D warnings` passed (artifact://338). Release build passed after the final dispatch fix (artifact://315). `--help` and `--version` actually ran and reported 0.1.0 (artifact://303). |
| Remote JSON-RPC smoke | Real release smoke exit 0: `connected created=2 turn1=lattice turn2_context=true second=marble switched=true reconnect_history=true`. Both created rows were deleted. |
| Corrected interrupt probe | First probe treated the interrupt ACK as terminal and recorded `quartz` as Interrupted with length 0. Corrected Stopping lifecycle probe exit 0: `connected created=1 interrupt=confirmed quartz=true resume_runtime=returned marked_user_once=1` (artifact://264). That disposable was deleted. |
| Native window, sidebar, composer | Actual `xwayland=false` release window. Sidebar, transcript, and bottom composer visually inspected at 918×850 and 1120×800 logical pixels; sidebar resized 285→340. |
| Context, distinct chats, cold resume | Native A produced two related `tessera` replies; the second prompt omitted that word. Enter sent; Shift+Enter inserted a newline. Native B produced a distinct `birch` reply. A cold client retrieved the same A four rows. |
| Read-only refusal | The owned Kanban fixture displayed its real 4132 explanation and both saved messages, with editor and Send disabled and no replacement chat. |
| Drafts, reload, streaming navigation | Per-session draft survived switch, read-only, return, and reconnect. During an actual reload, Send and Reload went disabled then enabled, with no accidental duplicate send. During a real eight-second terminal command the editor accepted typing, the sidebar scrolled, another owned chat was selected, and background completion did not steal it. Reconnect during that accepted command retained exactly one user prompt in authoritative history and did not replay it. |
| Selection and Stop | Hover did not select history. New stayed selected while the pointer was stationary during a model splice. Double-click selected C; Down then selected B without stealing editor focus; Enter focused the editor. Native Stop produced the Stopping name and Stop sensitive=0 before terminal Send sensitive=1. The preserved next-turn draft later completed as `quartz`. One non-atomic AT-SPI assertion raced true completion; that race is not a product failure. |
| Gateway noise and late cancellation | An unknown `cron.changed` broadcast no longer false-fails the chat. A rejected interrupt preserves busy state. A real gateway error retires the turn. An interrupt ACK is Interrupting/Stopping and stays busy until the actual `message.complete` or error. A late delta, ACK, interaction, or queued answer cannot reopen it. Late requests and answers are cancelled with existing RPC error `-32000` cancelled and are never granted after Stop. |
| Rich layout and copy | Screenshots `/tmp/hermes-native-rich-corrected.png` and `/tmp/hermes-native-rich-heading.png` were actually read: heading, bold, italic, strike, nested lists, quote, safe link versus inert unsafe link, literal `<span>` markup, monospace code, and normal-sized table headers and cells. Giant table height was fixed with non-wrapping natural cells and horizontal overflow. Mouse selection put exactly `Literal` on PRIMARY. Copy-source clipboard was exactly 429 characters and copy-code exactly 48, with a sentinel before each click. Ctrl+C prose clipboard copy did not pass and is not claimed. AT-SPI character-bounds and focus APIs are limited; that is not a product exception. |
| 300-row history | Native history loaded all 300 seeded rows (108,138 content characters). Earliest loaded row boundaries were 251, 201, 151, 101, 51, then 1; the older button then disappeared. There is no silent 200-row cap. Widgets were recycled; loaded data was retained. |
| Clarification | The native four-question card was actually submitted. Authoritative tool outcome was `submitted`: q0 `Blue`, q1 `[WebSocket,HTTP]`, q2 `live test`, q3 skipped/null. Radio, multiple, text, and explicit per-question Skip were exercised by pointer and native entry/actions. Earlier requests timed out during driver and GVariant diagnosis; those earlier requests were not answered. |
| GVariant action shape | `Variant::from` on the tuple inlined the third field as `(sss)`, `(ssb)`, or `(ssa{sms})`. The typed action expects `(ssv)`. `Tuple::to_variant` boxes the third field. A real GLib roundtrip proved all three shapes, and the actual native answers arrived. |
| Harmless approval probe | `bash -c 'echo HERMES-NATIVE-APPROVAL-OK'` succeeded with that output, exit 0, and no error. The server did not emit a live approval card. Shared string action serialization was proven, but a live card is not claimed. No policy change or dangerous command was used to force one. |
| Auth and unreachable profiles | Wrong password: `Authentication failed`, no running indicator, Send disabled, editor `editable=false`. Unreachable: the same configured host on port 9 with dummy credentials showed `Disconnected`, no running indicator, Send disabled, and was distinct from authentication failure. Neither profile created a chat. The temporary launcher was removed. |
| Same-process RSS | Release pid 402166 only. See the snapshot table below. Earlier-build snapshots are superseded and omitted. |
| Cleanup | Seven long-lived fixtures deleted. HTTP history 404 and absence from recent metadata are in `cleanup-verification.json`, status `completed`, verified 2026-11-09T08:11:05.796Z. Failed earlier smoke ids were separately confirmed absent. No leftovers. |

## Same-process RSS

Release pid 402166. These are single snapshots of resident set size, not peaks, PSS, GPU memory, isolated allocations, or a fixed budget. The three readings are different cache-warm states of the same process.

| Snapshot | RSS | Cache context |
|---|---:|---|
| Idle, before any transcript | 67,992 KiB | Connected, no transcript loaded |
| Fully loaded 300-row history | 87,196 KiB | All 300 seeded rows retained in process |
| Nonempty live text stream | 88,144 KiB | Taken after the 300-row history was already cached; the requested 400-line stream was stopped after this snapshot |

## Disposable fixtures

All seven long-lived ids below were prompted or otherwise owned, then deleted. Each history request returned HTTP 404, and none remained in recent metadata. The machine receipt is `.context/2026-11-08.hermes-native-client/cleanup-verification.json`. Do not treat any of them as still kept for later proof.

| Stored id | What it was | Current state |
|---|---|---|
| `20261108_211438_d4bbb9` | 60-row rich Markdown fixture `hn-native-verification-1794190478805` | Deleted; history 404 |
| `20261108_215025_f5ed1c` | `api_server` two-row fixture; source alone was not an eligibility boundary | Deleted; history 404 |
| `20261108_220055_7201bb` | `kanban` two-row fixture; resume refused with 4132 | Deleted; history 404 |
| `20261108_230027_3f372e` | Native A, four `tessera` rows | Deleted; history 404 |
| `20261109_003944_331a89` | Native B, `Birch`; terminal turn, reconnect, later submitted clarification | Deleted; history 404 |
| `20261108_235343_ad4267` | 300-row fixture `hn-long-history-300-1794200023308` | Deleted; history 404 |
| `20261109_015125_5f55da` | Native C, stream/Stop/`quartz` | Deleted; history 404 |

Earlier smoke ids `20261108_223852_36d027` and `20261108_223852_38902a` were confirmed absent by HTTP 404 after a failed cleanup report. `20261108_224529_13812e` and `20261108_224529_4bf1f3` were absent from owned fixture metadata; a raw delete of the second returned RPC 4007 `session not found`. That warning is not evidence of a leaked row.

An earlier raw cleanup socket stalled and its worker was lost. The completed receipt is from a fresh cookie and ticket socket that found all seven still present, deleted them, and verified all seven. The stalled attempt is not evidence.

## Corrections retained

These were real failures or harness limits. They are not current product defects.

- The first smoke masked its run result when cleanup failed. Later smoke preserved both errors, persisted the second `marble` chat, and deleted both rows.
- Initial async-constructor and GtkLabel wrap compile errors were corrected before the passing gates.
- The first interrupt probe treated an ACK as terminal. Typed Stopping until `message.complete` or error fixed it; the corrected probe passed.
- A fuzzy AT-SPI lookup only read the user's separate Hermes app. No action was sent there. The driver was corrected to the exact app name `hermes-native`.
- Eval had no compositor environment. The verification kernel derived the current Hyprland instance and `wayland-1`. User dotfiles were not changed.
- Native rich prose is AT-SPI paragraph text, not an accessible name. Name-only waits falsely timed out; direct text reads and screenshots confirmed the replies.
- Pointer automation must validate the scroll viewport. A clipped offscreen row was not treated as a successful switch.
- Early keyboard-focus troubleshooting previewed one unverified historical row. No prompt or deletion was sent there. Only tracked disposable conversations were prompted or deleted. It is false to say no historical row was ever opened.
- An immediate post-reload sensitivity check raced asynchronous GTK activation. Later event proof showed Send and Reload false→true while the draft was preserved.
- The first four-question request expired during driver troubleshooting and was recorded `timed_out`. A later request, after the GVariant fix, was submitted. Only the later request is an answered clarification.
- Streaming row replacement can invalidate an AT-SPI paragraph between traversal and read. The driver skips defunct nodes. That is not a client exception or history-loss claim.
- One Stop assertion raced real terminal completion between separate AT-SPI reads. Captured events still show Stopping, Stop sensitive=0, then Send sensitive=1. The draft then completed as `quartz`.
- `Variant::from` on an action tuple did not box the third field. `to_variant` does. The throwaway roundtrip was removed after proof.

## Closeout limits

- Credentials, tickets, cookies, gateway address, and signing secrets are not recorded. `.env` was not edited.
- Ctrl+C prose clipboard copy did not pass and is not claimed. PRIMARY selection and the source/code copy actions are the exercised copy proof.
- A live approval card was not observed. Code review and shared string serialization do not substitute for that card.
- Screenshots remain under `/tmp` as review evidence. Native verification apps were closed normally. The owned `ydotoold` was stopped, the original clipboard restored, and AT-SPI listeners removed. No source scaffolds remain.
- SQL memory is not configured. The session uses portable file memory, not a SQL receipt.
