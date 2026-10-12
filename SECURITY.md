# Security policy

Murmur v0.1.0 is distributed as a Developer ID–signed, Apple-notarized DMG for Apple Silicon macOS. It requires macOS 14.4 or later; installed functional testing was performed on macOS 27.0.1, not every version in that range. Intel Macs, Windows and Linux are not supported. Report issues against the latest GitHub release; no fixed support lifetime or security-response deadline is promised. In-app updates are disabled, so check GitHub Releases for manual updates.

**Do not open a public issue for a vulnerability.** Issues, comments and attachments are public. Never post transcripts, recordings, API keys, signing material, logs containing personal data, or details that expose someone else's system. Do not upload a diagnostic report without reading and redacting it yourself.

Private vulnerability reporting is enabled for this repository. Use **Security → Report a vulnerability**, or [start a private advisory](https://github.com/monkeyman1979/murmur/security/advisories/new). Give minimal reproduction steps using synthetic data, the affected version/commit and potential impact; do not attach real user data. The in-app bug-report link is for ordinary issues, not confidential vulnerability reports.

Dependency audits are not an assurance that all dependencies are vulnerability-free. Known development/build-tool findings are tracked separately from the shipped Apple Silicon runtime; do not execute development tools on untrusted projects. Signing and notarization establish distribution trust, not security or transcription correctness.
