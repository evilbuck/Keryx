# Changelog

## Unreleased

### Added

- Attach UTF-8 text files and text-bearing PDFs to chat messages. Their extracted text is included in conversation context alongside existing image attachments. Files can be selected with **Attach** or Ctrl+U, or dragged into the composer.

### Limitations

- Up to six files can be attached per message. Each file is limited to 15 MB, and extracted document text is limited to 1 MB per file. Unsupported binary files and PDFs without extractable text are rejected; scanned PDFs require OCR first.
