# Build, configure, and launch

Produce the release binary, point it at a remote Hermes dashboard, and open the window from the project directory.

You do not install or start a local Hermes. Startup loads `./.env` from the working directory only, without overriding variables already set in the process, and without searching parent directories.

## Steps

1. Install GTK 4 (4.16 or newer), libadwaita (1.6 or newer), `pkg-config`, and a C build toolchain. Rust 1.99.1 with GTK 4.24.1 and libadwaita 1.10.0 is the tested set. This repo does not declare a minimum supported Rust version.
2. From the project directory, create the configuration only if it does not already exist, then restrict its permissions:

   ```sh
   test -e .env || cp .env.example .env
   chmod 600 .env
   ```

3. Edit `.env`. Set `HERMES_GATEWAY_URL` to the dashboard origin (`https://…`, no path, user, query, or fragment), plus `HERMES_USER` and `HERMES_PASSWORD`. Leave the example file unchanged. Do not put an API key or `HERMES_BASIC_AUTH_SECRET` in this file.
4. Build the release binary:

   ```sh
   cargo build --release
   ```

5. Stay in the project directory and start it:

   ```sh
   ./target/release/hermes-native
   ```

   Add `--trusted-tunnel` only when the URL is `http` on loopback or a Tailscale host (`*.ts.net`, 100.64.0.0/10, or `fd7a:115c:a1e0::/48`) and you have confirmed that hop is inside an encrypted tunnel you trust. Public HTTP is rejected. The flag does not turn off HTTPS certificate checks and does not accept other private-IP ranges.
6. **Eat:** the window shows the session sidebar, the header reads **Connected**, and the composer hint is **Enter to send · Shift+Enter for a new line**.

If the header reads **Not configured**, the three variables were empty or `.env` was not in this directory. **Authentication failed** means the remote rejected the password. **Disconnected** means the socket did not stay up; use the reconnect button. Nothing you already sent is replayed.

`hermes-native --help` prints the same flag rule. `--version` prints the package version.
