Native Rust client for a remote Hermes dashboard. GTK 4 and libadwaita draw a resizable session sidebar, a selectable Markdown transcript, and a bottom composer. There is no browser runtime and no local Hermes install.

The remote server named in your config is the only gateway this client talks to. It signs in with a password, keeps the session cookie, and opens the dashboard JSON-RPC WebSocket. Conversation IDs stay server-owned.

## Requirements

- GTK 4 **4.16 or newer**, libadwaita **1.6 or newer**, `pkg-config`, and a C toolchain. Crates pin those minimums.
- Tested with Rust **1.99.1**, GTK **4.24.1**, and libadwaita **1.10.0**. This repo does not declare a minimum supported Rust version.
- A Wayland session for the desktop window. No desktop file, package, or settings screen is included.

## Run

Build, configure, and launch from [Run Hermes Native](docs/howto/run-hermes-native.md). Copy [`.env.example`](.env.example); do not put real passwords in the example.

`HERMES_GATEWAY_URL` is an HTTPS dashboard origin. Plain HTTP is rejected unless you pass `--trusted-tunnel` **and** the host is loopback or Tailscale. That flag does not disable certificate checks and does not allow arbitrary public HTTP or other private-IP ranges.

## Layout

- [Architecture](docs/architecture.md) — widgets, transport, history, and check commands.
- [How-to guides](docs/howto/README.md) — the build and run sequence.
