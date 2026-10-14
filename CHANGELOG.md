# Changelog

Notable changes to Murmur are listed here. Versions follow [Semantic Versioning](https://semver.org/).

## [0.1.1] - Unreleased

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

[0.1.1]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.1
[0.1.0]: https://github.com/monkeyman1979/murmur/releases/tag/v0.1.0
