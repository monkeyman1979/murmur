# Murmur

Murmur is a local-first macOS menu-bar app for voice dictation and meeting transcription. The app is free and does not require an account, purchase, license key, or cloud transcription. Dictation inserts text into another app via the clipboard and macOS paste automation; Meetings can save local transcripts and optional retained audio.

## Download

**[Download for Apple Silicon Mac (.dmg)](https://github.com/monkeyman1979/murmur/releases/download/v0.1.0/Murmur_0.1.0_aarch64.dmg)**

**Requires macOS 14.4 or later. Not compatible with Intel Macs.** Windows and Linux are not supported. The v0.1.0 installer is Developer ID–signed, Apple-notarized and stapled; its checksum is included in the [GitHub Release](https://github.com/monkeyman1979/murmur/releases/tag/v0.1.0).

Installation and functional checks were performed on Apple Silicon with **macOS 27.0.1 (26A434)**. The macOS 14.4 floor is the configured requirement, not a claim that every older supported macOS version has been independently tested.

1. Open the downloaded DMG and drag **Murmur** into **Applications**. For a manual update, quit Murmur first and choose **Replace**; do not delete your app data.
2. Eject the DMG and open **Murmur** from Applications. macOS may show its normal downloaded-app confirmation.
3. Complete onboarding: allow the microphone, turn on Murmur in **System Settings → Privacy & Security → Accessibility**, and download the speech model (~488 MB). Input Monitoring is requested only if needed for your shortcut. After the model download, transcription runs locally.

Murmur lives in the menu bar. Use its menu to open the window or **Quit Murmur**; closing the window does not quit it. **Launch at login** is optional. Manual launches open the window; login startup stays quiet after onboarding.

### First meeting and updates

Meeting capture records your microphone and the Mac's system audio, not video. Ask participants for consent. The first meeting can trigger macOS **System Audio Recording** and **Documents** permission prompts; Markdown transcripts default to `Documents/Murmur/Meetings`. Microphone capture can begin while the first system-audio prompt is awaiting your answer. A folder-save failure offers another folder and retry, but denied-permission/recovery paths have not all been independently exercised.

Automatic/manual in-app update checks are disabled in v0.1.0. Download updates from [GitHub Releases](https://github.com/monkeyman1979/murmur/releases) and replace the app manually. A retained-data update between the tested release candidates preserved local data and model files; migration from every historical development build and all interrupted-recording recovery cases has not been validated.

## Languages

Settings → General → App language offers English, Spanish, French and Russian interface text for trial; independent fluent review and installed-app localization QA remain outstanding. The interface preference does not set transcription language or rewrite saved transcripts. Dictation and new Meetings use Whisper multilingual Auto-detect, with no manual language picker. Saved legacy meeting processing settings are preserved on resume. The Dictation language information dialog lists model language tokens, **not** languages individually verified for recognition quality; Auto-detect does not guarantee accurate mid-meeting language switching. Copy for AI uses the current interface language for its unsaved default instructions, but preserves any saved custom instructions verbatim; it only prepares text for user-initiated handoff to an external AI, not automatic cloud transcription.

## Build and test

See [BUILDING.md](docs/BUILDING.md) for pinned tools, commands, permissions and source-build limitations. Murmur requires macOS 14.4+ on Apple Silicon; see the tested-version scope above. The Whisper model is downloaded only when requested and verified against a pinned size and SHA-256; it is not included in this source tree or app bundle.

## Privacy and safety

Transcription runs locally after model download. Microphone and Accessibility permissions enable the relevant features; Input Monitoring may also be needed for shortcuts on some macOS versions; meeting capture also requires consent from participants and may retain sensitive audio/transcripts on disk. See [PRIVACY.md](docs/PRIVACY.md) for storage, clipboard, network access and deletion limitations. Never put transcripts, recordings, credentials or personal details in a public issue. Security reports belong in the private channel described in [SECURITY.md](SECURITY.md), using GitHub's enabled private vulnerability-reporting route.

## Participate

[CONTRIBUTING.md](CONTRIBUTING.md) explains tests, safe bug reports and contribution expectations. [SECURITY.md](SECURITY.md) explains private vulnerability reporting. Settings → About points to the [bug-report form](https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml). The onboarding privacy link opens [docs/PRIVACY.md](docs/PRIVACY.md) on GitHub in a browser; it is not an offline in-app page. Automatic/manual in-app update checks and updater artifacts are disabled for this release; use GitHub Releases for manual downloads.

Original Murmur material is [MIT licensed](LICENSE). Third-party components retain their own terms: see [bundled notices](src-tauri/resources/THIRD-PARTY-NOTICES.txt) and the [dependency inventory](third-party/dependency-inventory.json). Complete license texts stay in those separate files, not the in-app Acknowledgements summary. The downloaded speech model is separate from the installer and retains its own license.
