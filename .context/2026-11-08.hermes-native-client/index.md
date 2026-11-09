---
status: completed
lifecycle_schema: 1
lifecycle_revision: 3
lifecycle_last_transition: close-verified
---

# Hermes native client

Native Hyprland client for a remote Nous Research Hermes server.

## Artifacts
- [Requirements and intake draft](brainstorm-hermes-native-client.md)
- [Implementation plan](plan-native-client.md)
- [Research summary](research-native-client.md)
- [Connection architecture](research-connection.md)
- [Rolling notes](research/notes-native-client.md)
- [Sources](research/sources-native-client.md)
- [Product specification](spec-hermes-native-client.md)
- [Proposed implementation contract (draft sketch, not implementable authority)](implementation-contract.md)
- [Connection memory checkpoint](../memory/hermes-connection-2026-11-08.md)
- [Implementation memory](../memory/hermes-native-implementation-2026-11-09.md)
- [Implementation evidence](verification-implementation.md)
- [Fixture cleanup receipt](cleanup-verification.json)
- [Integration corrections](iterate-native-client.md)
- [Observed runtime contract](research-runtime-contract.md)

## Current boundary

The Rust/GTK4/libadwaita client is implemented and verified against the configured remote gateway only. Server history is authoritative: eligible sessions resume, ineligible sessions stay read-only, and accepted prompts are not replayed. Markdown is per-block selectable native text, not a webview. Credentials stay out of artifacts. No local Hermes target, installer, or leftover disposable fixtures.

Evidence: [verification-implementation.md](verification-implementation.md). Cleanup: [cleanup-verification.json](cleanup-verification.json) (seven long-lived fixtures deleted; no leftovers). Plan, spec, work records, and session memory are completed. Git records commit identity.

Conditional limit: a live approval card was not observed under server policy. Code review and shared string action serialization cover that path. Do not treat the card as live-approved.
