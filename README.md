Native Rust client for a remote Hermes dashboard. [GPUI](https://www.gpui.rs/) 0.2.2 draws the session sidebar, Markdown transcript, and composer. There is no browser runtime and no local Hermes install.

The remote server named in your config is the only gateway this client talks to. It signs in with a password, keeps the session cookie, and opens the dashboard JSON-RPC WebSocket. Conversation IDs stay server-owned.

## Requirements

- A C toolchain with `g++` (GPUI links `libstdc++`), `pkg-config`, and the Wayland/X11 development packages GPUI 0.2.2 compiles against: `libwayland-dev`, `libxkbcommon-dev`, `libxkbcommon-x11-dev`, and `libxcb1-dev`.
- At runtime: a Wayland or X11 session, `fontconfig` fonts, `libvulkan1`, and a Vulkan driver. `mesa-vulkan-drivers` supplies lavapipe for a headless or software display.
- GPUI is pinned to the crates.io **0.2.2** snapshot (published 2025-10-22), vendored at `vendor/gpui` with its event-dispatch result made public for agent input. Zed's `main` has since diverged. Default features include the Linux Wayland and X11 backends. `third_party/xattr` patches Linux `ENOATTR` compatibility.
- Tested with Rust **1.100.0**. This repo does not declare a minimum supported Rust version. GPUI 0.2.2 itself is edition 2024.
- `--preview` draws the window against a loopback stub. A real session still wants a Wayland or X11 display.

## Run

Build, configure, and launch from [Run Hermes Native](docs/howto/run-hermes-native.md). Copy [`.env.example`](.env.example); do not put real passwords in the example. `.env` is gitignored.

`HERMES_GATEWAY_URL` is an HTTPS dashboard origin. Plain HTTP is rejected unless you pass `--trusted-tunnel` **and** the host is loopback or Tailscale. That flag does not disable certificate checks and does not allow arbitrary public HTTP or other private-IP ranges.

## Agent control (experimental)

Start with `--agent-socket PATH`, where the socket's existing parent directory is owned by the current user and has mode `0700`. Socket activation requires Linux 6.6 or later (`fchmodat2`). The socket is owner-only and accepts newline-delimited JSON requests such as `{"command":"state"}`, `{"command":"text","text":"hello"}`, and `{"command":"key","key":"shift-enter"}`. The default is off. Anyone with access can read drafts and operate the application, including actions that affect the remote server.

If a connection closes, times out, or returns an incomplete response after a command is sent, its outcome is unknown. Do not retry an action as though it definitely did not happen.

`{"command":"click","x":95,"y":292}` sends a left click in window-relative logical pixels; screenshot pixels may be scaled. `{"command":"screenshot","path":"/absolute/path/to/new.png"}` requires Hyprland and `grim` with `-T` support. It captures the application's toplevel without focusing it and refuses existing paths.

## Layout

- [Architecture](docs/architecture.md) — views, transport, history, and check commands.
- [How-to guides](docs/howto/README.md) — the build and run sequence.
