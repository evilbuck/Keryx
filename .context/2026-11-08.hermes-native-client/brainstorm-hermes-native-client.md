---
status: draft
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
research: [research-native-client.md]
---
# Hermes native client

## User Goal
Give the user a fast, low-memory native desktop interface on Hyprland for talking to a remote Nous Research Hermes agent, with multiple conversations and readable rich-text responses.

## Requested behavior
- Connect to a remote Hermes server rather than run an agent locally.
- A sidebar lists chats/sessions and provides a new-chat action.
- Selecting a conversation displays its history; the user can send messages and receive replies.
- Render rich text in the transcript, not just a plain-text terminal dump.
- Implement in C or Rust. No Electron, Tauri, embedded browser, or JavaScript application runtime.
- Use canonical generators for generated project/protocol code.

## Layout reference
`/home/buckleyrobinson/Pictures/screenshots/screenshot-20261108-191503.png` is inspiration, not a literal design contract.

The reference suggests a restrained dark interface, persistent conversation sidebar, spacious readable transcript, and bottom-anchored composer. Its extra navigation rail, scheduled-task cards, voice/call controls, and product-specific integrations are not requested features.

## Initial product boundaries
- Conversation list, create, select/resume, transcript, composer, connection setup/status.
- Markdown is the working interpretation of rich text: paragraphs, emphasis, headings, lists, links, quotations, inline/fenced code, and tables. Exact supported syntax must be stated rather than silently dropped.
- Network and agent work must not block the UI thread.
- Session ownership, discovery, authentication, streaming, and history must follow the actual Hermes remote protocol; do not invent endpoints or assume compatibility from the word "server".
- Do not display fake example conversations as live server data.

## Research outcome
- Recommend Rust + GTK4/libadwaita for native Wayland UI and `pulldown-cmark` for native rich-text rendering.
- Hermes already exposes official remote interfaces; no custom Python bridge is justified.
- Recommend the remote dashboard backend for existing eligible Hermes sessions. Its canonical WebSocket protocol supports session resume/prompt and interactive controls; HTTP routes support sidebar/history.
- The API server is also official but its session ownership differs. API-created-only chats are a distinct product choice, not a transparent substitute for existing-session support.
- Canonical generators: Cargo project creation, standard GTK resource generation, and types derived from pinned upstream gateway schemas.
- Details and acceptance criteria: [spec-hermes-native-client.md](spec-hermes-native-client.md).

## Repository evidence
- Initial project directory contains only AGENTS.md; there is no application, historical memory, backlog, or Git repository to preserve.
- Project constraints: Hyprland, low-overhead languages, no Electron/Tauri, web research before decisions, canonical generators.
- Rust/Cargo and pkg-config are installed. pkg-config reports GTK 4.24.1 and libadwaita 1.10.0. GNOME Builder and Meson were not found by command lookup.
- SQL memory tools are exposed, but no project origin or Git-common-directory identity can be derived because the directory is not a Git repository. No SQL recall was attempted under a guessed identity.

## Material questions
Should this client browse and resume existing eligible Hermes sessions on the remote server, or only handle API-created conversations? The answer selects the required remote service and initial protocol.
