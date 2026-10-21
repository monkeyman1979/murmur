# Changelog

Notable changes to Murmur are listed here. Versions follow [Semantic Versioning](https://semver.org/).

## [0.1.3] - 2026-10-21

### Improved

- The dictation start cue now plays shortly after the microphone begins delivering audio, making it more reliable on tested Bluetooth earbuds without delaying capture or the recording indicator.
- Dictation now closes the microphone stream before collecting the final audio buffer, reducing the chance of dropping audio at key release. A stale auto-stopped capture no longer blocks a later dictation attempt.

Bluetooth dictation can still miss speech begun immediately at the hotkey press, and this update does not change Bluetooth playback quality. Wait for the start cue before speaking.

## [0.1.2] - 2026-10-15

### Added

- In-app updates. Murmur checks GitHub Releases at launch, at most once a day, and shows a new version in the sidebar above Settings, or as a download icon when the sidebar is collapsed. Settings → About can also check, download and restart to update. Each update is verified against Murmur's updater signing key, and the restart waits for any recording or transcription to finish.

### Changed

- Successful meeting audio is now kept for at most 7 days. **Keep successful meeting audio for** offers Delete immediately, After 24 hours (the default) or After 7 days; **Until deleted** and the per-meeting **Keep until deleted** action are gone. To keep a recording, use **Save audio copy**. Failed or interrupted recordings are still kept until you retry, save a copy or delete them.
- Settings → General explains that meeting audio is stored privately inside Murmur, separate from your transcript folder. **Delete meeting history now** is now **Delete all meetings from Murmur**, and its description says it removes transcripts and audio inside Murmur but leaves your Markdown files and saved audio copies.
- Settings → Permissions lists Microphone and Accessibility, and shows Input Monitoring only if your shortcut still isn't detected after Accessibility is granted. On macOS 27, Accessibility also covers the shortcut, so Input Monitoring no longer sends you to a System Settings pane without Murmur in it. The permission descriptions are plainer.
- The meeting actions menu (⋯) widens to fit its longest item, so Spanish, French and Russian labels no longer crowd.

### If you're updating from v0.1.1

- v0.1.1 can't check for updates. Download v0.1.2 from GitHub Releases and replace the app; later versions can update from inside Murmur.
- If you chose **Until deleted** for meeting audio, Murmur switches it to After 7 days, and audio you kept is deleted 7 days after its recording ended. Before updating, use **Save audio copy** on any recording you want to keep.

## [0.1.1] - 2026-10-14

### Fixed

- The app now installs correctly on Macs with more than one user account. The v0.1.0 installer gave `Murmur.app` owner-only file permissions, so after a different account installed or replaced it, other accounts could no longer open Murmur.
- Launch at login now appears in System Settings → General → Login Items as **Murmur** under Open at Login. In v0.1.0 it was a background item that macOS attributed to the developer's name.

### Changed

- If Launch at login was on in v0.1.0, it stays on after updating. Murmur moves it to the new login item and removes the old `~/Library/LaunchAgents/Murmur.plist`.
- Release checks now fail if any file in the app bundle cannot be read by every user account.
- Removed the outdated "Pre-release build — not an official public download" and "source-only candidate" wording from Settings → About.

### If you're updating from v0.1.0 on a shared Mac

If Murmur stopped opening for one account after another account installed it, run this once from an administrator account, then reinstall from the v0.1.1 DMG:

```
sudo chown -R root:admin /Applications/Murmur.app && sudo chmod -R u+rwX,go+rX,go-w /Applications/Murmur.app
```

## [0.1.0] - 2026-10-12

First public release: a Developer ID–signed, notarized DMG for Apple Silicon Macs running macOS 14.4 or later.

[0.1.3]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.3
[0.1.2]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.2
[0.1.1]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.1
[0.1.0]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.0
