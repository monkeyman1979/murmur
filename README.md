# Murmur

Murmur is a local-first macOS menu-bar app for voice dictation and meeting transcription. Source builds do not require an account, purchase, license key, or cloud transcription. Dictation inserts text into another app via the clipboard and macOS paste automation; Meetings can save local transcripts and optional retained audio.

**Release status:** This repository is a *preparation snapshot*, not an official download or a published v0.1.0 release. The first release is provisionally **v0.1.0, macOS (Apple Silicon) only**; Linux support requires separate implementation and QA. No signed/notarized installer is offered here. Build from source for development; an unsigned local app is not a trusted public download.

## Languages

Settings → General → App language offers English, Spanish, French and Russian interface text for trial; independent fluent review and installed-app localization QA remain outstanding. The interface preference does not set transcription language or rewrite saved transcripts. Dictation and new Meetings use Whisper multilingual Auto-detect, with no manual language picker. Saved legacy meeting processing settings are preserved on resume. The Dictation language information dialog lists model language tokens, **not** languages individually verified for recognition quality; Auto-detect does not guarantee accurate mid-meeting language switching. Copy for AI uses the current interface language for its unsaved default instructions, but preserves any saved custom instructions verbatim; it only prepares text for user-initiated handoff to an external AI, not automatic cloud transcription.

## Build and test

See [BUILDING.md](docs/BUILDING.md) for pinned tools, commands, permissions and source-build limitations. Murmur targets macOS 14.4+ on Apple Silicon for this candidate. The Whisper model is downloaded only when requested and verified against a pinned size and SHA-256; it is not included in this source tree or app bundle.

## Privacy and safety

Transcription runs locally after model download. Microphone, Input Monitoring and Accessibility permissions are needed for the relevant features; meeting capture also requires consent from participants and may retain sensitive audio/transcripts on disk. See [PRIVACY.md](docs/PRIVACY.md) for storage, clipboard, network access and deletion limitations. Never put transcripts, recordings, credentials or personal details in a public issue. Security reports belong in the private channel described in [SECURITY.md](SECURITY.md), once the repository owner enables it.

## Participate

[CONTRIBUTING.md](CONTRIBUTING.md) explains tests, safe bug reports and contribution expectations. [SECURITY.md](SECURITY.md) explains private vulnerability reporting. Settings → About points to the [bug-report form](https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml). The onboarding privacy link opens [docs/PRIVACY.md](docs/PRIVACY.md) on GitHub in a browser; it is not an offline in-app page. The repository owner must verify both links before directing users to the public repository. Automatic/manual update checks and updater artifacts are paused in the source-only production build; the retained endpoint/key are not approved for public distribution.

Original Murmur material is [MIT licensed](LICENSE), subject to ownership review before publication. Third-party components retain their own terms: see [bundled notices](src-tauri/resources/THIRD-PARTY-NOTICES.txt) and the [dependency inventory](third-party/dependency-inventory.json). Complete license texts stay in those separate files, not the in-app Acknowledgements summary. Exact-snapshot rights/asset review remains open.
