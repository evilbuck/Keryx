# Changelog

## Unreleased

### Added
- Automatic reconnect with exponential backoff. After an unintentional socket loss the client retries in 1s, doubling to a 30s cap, and drains queued commands once the gateway is ready again. Bad-password and rate-limited logins are not retried automatically.

- Attach UTF-8 text files and text-bearing PDFs to chat messages. Their extracted text is included in conversation context alongside existing image attachments. Files can be selected with **Attach** or Ctrl+U, or dragged into the composer.

- Group assistant text and tool activity in one response card. A running tool shows a short activity line with a type-specific icon and a redacted preview when the gateway sends one; on completion the line stays as a finished notation. Saved tool rows show the tool name, not arguments or output.

### Limitations

- Up to six files can be attached per message. Each file is limited to 15 MB, and extracted document text is limited to 1 MB per file. Unsupported binary files and PDFs without extractable text are rejected; scanned PDFs require OCR first.
