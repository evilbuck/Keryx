---
date: 2026-11-18
domains: [frontend, backend]
topics: [transcript, tool-calls, streaming]
related: [src/ui.rs, src/model.rs, docs/architecture.md]
priority: medium
status: active
---

# Inline tool activity

Assistant text and tool notations are grouped in one response card; live tool starts and completions are inserted into the transcript, and saved tool results use the same presentation. Streamed text is segmented around tool events. Search and outline navigation map source indices to grouped rows. Research: progressive disclosure and non-disruptive status updates are supported by https://design-system.service.gov.uk/components/details/ and https://www.w3.org/WAI/WCAG22/Techniques/aria/ARIA22.html. The durable guardrails passed: 105 library tests, remote smoke, lint, complexity, and 56.3% coverage. Native GTK layout was not visually verified.
