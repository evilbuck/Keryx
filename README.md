Native Rust client for a remote Hermes dashboard. [GPUI](https://www.gpui.rs/) 0.2.2 draws the session sidebar, Markdown transcript, and composer. There is no browser runtime and no local Hermes install.

The remote server named in your config is the only gateway this client talks to. It signs in with a password, keeps the session cookie, and opens the dashboard JSON-RPC WebSocket. Conversation IDs stay server-owned.

## Requirements

- A C toolchain with `g++` (GPUI links `libstdc++`), `pkg-config`, and the Wayland/X11 development packages GPUI 0.2.2 compiles against: `libwayland-dev`, `libxkbcommon-dev`, `libxkbcommon-x11-dev`, and `libxcb1-dev`.
- At runtime: a Wayland or X11 session, `fontconfig` fonts, `libvulkan1`, and a Vulkan driver. `mesa-vulkan-drivers` supplies lavapipe for a headless or software display.
- GPUI is the crates.io crate `gpui` **0.2.2** (published 2025-10-22). Zed's `main` branch still says `0.2.2` but has moved on, so this repo pins the registry snapshot whose `Application::new` API matches [gpui.rs](https://www.gpui.rs/) and [docs.rs/gpui/0.2.2](https://docs.rs/gpui/0.2.2). Default features include the Linux Wayland and X11 backends. `third_party/xattr` is a one-line patch of `xattr` 0.2.3 so it builds with libc 0.2.191, which dropped `ENOATTR` on Linux.
- Tested with Rust **1.100.0**. This repo does not declare a minimum supported Rust version. GPUI 0.2.2 itself is edition 2024.
- `--preview` draws the window against a loopback stub. A real session still wants a Wayland or X11 display.

## Run

Build, configure, and launch from [Run Hermes Native](docs/howto/run-hermes-native.md). Copy [`.env.example`](.env.example); do not put real passwords in the example. `.env` is gitignored.

`HERMES_GATEWAY_URL` is an HTTPS dashboard origin. Plain HTTP is rejected unless you pass `--trusted-tunnel` **and** the host is loopback or Tailscale. That flag does not disable certificate checks and does not allow arbitrary public HTTP or other private-IP ranges.

## Layout

- [Architecture](docs/architecture.md) — views, transport, history, and check commands.
- [How-to guides](docs/howto/README.md) — the build and run sequence.
