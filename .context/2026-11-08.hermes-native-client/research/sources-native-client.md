---
status: active
date: 2026-11-08
subject: 2026-11-08.hermes-native-client
topics: [hermes, native-ui, wayland, remote-api]
informs: [../research-native-client.md]
---
# Research sources

## User-provided references
- Hermes documentation entrypoint: https://hermes-agent.nousresearch.com/ — user supplied; detailed API facts not yet verified.
- Layout image: `/home/buckleyrobinson/Pictures/screenshots/screenshot-20261108-191503.png` — viewed directly on 2026-11-08; visual inspiration only.

## Local evidence (2026-11-08)
- Project directory listing: only AGENTS.md before context initialization.
- Project AGENTS.md supplied by the environment: Hyprland, low overhead, no Electron/Tauri, web research, canonical generators.
- `command -v cargo rustc pkg-config gnome-builder meson gtk4-builder-tool`: Cargo, rustc, pkg-config, gtk4-builder-tool found; GNOME Builder/Meson absent.
- `pkg-config --modversion gtk4 libadwaita-1`: 4.24.1 / 1.10.0.
- `git remote get-url origin`: fails because this directory is not in a Git repository. This is not evidence of an empty SQL memory store.

## Official Hermes HTTP API (consulted 2026-11-08)
- https://raw.githubusercontent.com/nousresearch/hermes-agent/main/website/docs/user-guide/features/api-server.md
- Describes an official HTTP API, `API_SERVER_ENABLED`, `API_SERVER_KEY`, `hermes gateway`, default port 8642, SSE, and stateful Responses API chains.
- All SSE streams can emit `: keepalive`; clients must handle SSE comment frames rather than parse them as JSON.
- Main-branch documentation is evidence of current upstream behavior, not proof of the user's deployed server version.

## Official dashboard/remote backend (consulted 2026-11-08)
- https://raw.githubusercontent.com/nousresearch/hermes-agent/main/website/docs/user-guide/features/web-dashboard.md
- \"The 'remote backend' Desktop connects to is a `hermes dashboard` process running on the remote machine.\"
- \"The live chat connection is a separate WebSocket to `/api/ws` (and `/api/pty`)\"; readiness alone does not authenticate it.
- Non-loopback dashboard fails closed without an auth provider; remote username/password setup and host-header/peer guards are documented.

## Verified canonical gateway ownership and schemas (researcher inspection, 2026-11-08)
- https://raw.githubusercontent.com/nousresearch/hermes-agent/main/website/docs/developer-guide/gateway-local-sessions.md — operator scopes, stored/live session discovery, explicit resume/adoption, and legacy/ownership limits.
- https://hermes-agent.nousresearch.com/docs/developer-guide/programmatic-integration — canonical WebSocket session/prompt methods, event streams, capabilities and interactive controls.
- https://github.com/NousResearch/hermes-agent/blob/main/hermes_cli/web_routers/chat_ws.py — dashboard `/api/ws` invokes canonical dispatch with operator scope.
- https://github.com/NousResearch/hermes-agent/blob/main/apps/shared/src/gateway-contract.openrpc.json — canonical generated OpenRPC method/type contract.
- https://github.com/NousResearch/hermes-agent/blob/main/scripts/gen_gateway_contracts.py — upstream generator for the canonical gateway artifacts.
- https://github.com/NousResearch/hermes-agent/blob/main/apps/desktop/src/api/sessions.ts — official desktop HTTP sidebar/history reference; not a public versioned OpenAPI artifact.
- https://github.com/NousResearch/hermes-agent/blob/main/apps/desktop/src/api/canonical-protocol.ts — official local/remote gateway credential handling.

Correction: a researcher's initial phrase \"no public remote chat API\" was incorrect and withdrawn. Hermes has a documented HTTP chat API. The absence identified was an upstream OpenAPI artifact, not an API implementation.

## Native UI primary sources (researcher inspection, 2026-11-08)
- https://docs.gtk.org/gtk4/wayland — native GDK Wayland backend.
- https://gtk-rs.org/gtk4-rs/stable/latest/book/ — official Rust GTK project setup, event loop, composite templates, list widgets and accessibility.
- https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/struct.Label.html — selectable labels, markup and links.
- https://docs.gtk.org/gtk4/signal.Label.activate-link.html — native link activation.
- https://world.pages.gitlab.gnome.org/Rust/libadwaita-rs/stable/latest/docs/libadwaita/struct.NavigationSplitView.html — responsive sidebar/content layout.
- https://world.pages.gitlab.gnome.org/Rust/sourceview5-rs/stable/latest/docs/sourceview5/ — native code-display widget and language/style support.
- https://docs.rs/pulldown-cmark/latest/pulldown_cmark/index.html — Markdown parse events and optional table/task-list extensions.
- https://docs.rs/iced/0.14.0/iced/widget/markdown/index.html — Iced built-in Markdown alternative.
- https://github.com/iced-rs/iced/issues/552 — accessibility limitation investigated for Iced; verify again before relying on this comparison in future work.
- https://github.com/GNOME/gnome-builder/tree/main/src/plugins/meson-templates/resources/src — official Rust GTK application templates; Meson/Builder not installed locally.
- https://gnome.pages.gitlab.gnome.org/blueprint-compiler — optional GTK UI generation; not a prerequisite for a Cargo/composite-template project.
