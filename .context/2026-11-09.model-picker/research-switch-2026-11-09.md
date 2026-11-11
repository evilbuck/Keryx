---
status: active
date: 2026-11-09
subject: 2026-11-09.model-picker
topics: [model-picker, remote-contract, session-switch]
informs: [plan-model-picker.md]
---
# Initialized-session switching

Disposable remote verification established that `config.set` with `deepseek-flash --provider custom:multi-model-gateway --session` after an initial turn switched subsequent inference to that model. The control session and global identity stayed on the original model. A cold resume reported the switched pair in the resume response and subsequent inference used it, but `model.options` before that inference incorrectly reported the global default. Both disposable sessions were closed and deleted; the session listing confirmed cleanup.

The earlier probe found a pre-first-turn `config.set` acknowledgment did not switch either subsequent inference. Session creation with an explicit model does work, but creates a new server-owned identity. The user requested default-model new chats and switching at any time. Replacing an existing empty server session would change its identity; user has not approved that tradeoff. Switching during a running turn is also not established as immediate and safe. No picker implementation was made.
