# Changelog

All notable changes to AstraTerm are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2026-11-26

### Added

- Kitty graphics sent in chunks (`m=1`) are reassembled and drawn. Clients that
  split an image across several escape sequences (`kitten icat`, `yazi`, `chafa`
  in kitty mode) now work instead of being refused.
- In-flight transfers are bounded: a per-transfer and aggregate byte budget, at
  most four live transfers, and the oldest one dropped when the budget runs out.
  A final chunk without a transfer, or a chunk without an image id, is an error
  rather than a silently truncated image.

## [1.0.0] - 2026-11-23

First public version: a native terminal emulator for Linux whose window, tabs
and splits are implemented in Rust on top of wgpu, with no Electron, webview or
tmux in the stack.

### Added

- Native terminal with real PTY sessions, scrollback and search.
- Tabs and recursive split panes managed by the application, plus a floating
  terminal that can be hidden and brought back.
- Themes, including the built-in `midnight` and `dawn` palettes and a
  keyboard-driven theme editor with live preview.
- Native settings panel that edits the same typed configuration model as the
  TOML file, with atomic saves and live reload of external edits.
- Kitty keyboard protocol support, so applications that ask for it receive
  unambiguous key events.
- Optional ligatures and optional inline images (kitty graphics protocol,
  iTerm2 `OSC 1337;File=`, sixel), both off by default and bounded in memory.
- Desktop notifications from the application through `OSC 9` and `OSC 777`.
- Clipboard integration: copy/paste of selections and bracketed paste, with
  application-driven clipboard access through `OSC 52` disabled by default.
- Save and reload notifications for the configuration.
- Command-line interface (`--working-directory`, `--profile`, `--command`,
  `--title`, `--config`, `--new-window`, `--debug`) and `--version`.
- Packaging: Arch `PKGBUILD`, a freedesktop desktop entry with a committed icon
  set in six sizes, and a rootless `packaging/linux/install.sh` /
  `uninstall.sh` pair for a per-user install.
- `scripts/setup.sh`, an idempotent bootstrap that installs the system
  dependencies for the detected distribution, checks the Rust toolchain and the
  default Nerd Font without downloading anything, builds with
  `cargo build --release --locked` and optionally installs the result.
  Documented in [`docs/INSTALL.md`](docs/INSTALL.md).

### Changed

- The project is versioned `1.0.0`; `astraterm --version` reports the version of
  the workspace package.

[Unreleased]: https://example.invalid/astraterm/compare/1.0.0...HEAD
[1.0.0]: https://example.invalid/astraterm/releases/tag/1.0.0
