---
date: 2026-11-18
domains: [frontend, backend]
topics: [attachments, pdf, chat-context]
related: [src/studio.rs, src/backend.rs, src/ui.rs, docs/architecture.md]
priority: medium
status: active
---

# Chat file attachments

The composer stages UTF-8 text and text-bearing PDFs alongside images. Document extraction occurs off the UI thread; extracted text is included in prompt.submit, while images retain the gateway image-upload and image.attach flow. Inputs are limited to 15 MB and extracted text to 1 MB per document; unsupported binary files and PDFs without extractable text are rejected. Scanned PDFs need OCR. The durable guardrails passed, including 104 library tests, live gateway smoke, lint, coverage and complexity. The new document flow was not visually verified.
