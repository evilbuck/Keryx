---
status: completed
informs: []
---

# Saved history display

Read-only inspection of the configured remote history on 2026-11-09 found that tool-result rows commonly contain JSON in `content` and identify the tool in `tool_name`. User and assistant text in the sampled pages was ordinary text. The transcript already renders message text through the native Markdown renderer; displaying tool results as ordinary messages exposed their JSON output.

History conversion now preserves each server row and its pagination cursor, but replaces tool-result content with `Used <tool_name>` or `Tool called` when no name is available. User and assistant text is unchanged. The focused `history_pages_render_messages_and_summarize_tool_results` test passed. The durable guardrails verdict passed unit tests, remote functional smoke, lint, coverage ratchet, and complexity; the patch gate was disabled. Native visual verification was unavailable because `orca-ide` is not installed.
