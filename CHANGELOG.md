# Changelog

All notable changes to CraftCenter are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.1]

### Fixed

- Self-update on macOS and Windows replaced the program with the downloaded archive — a disk
  image or a zip — instead of the program inside it. Each format is now unpacked the way an app
  install unpacks it.
- The previous build is now kept until the new one has started once, so a build that does not run
  can be gone back to. (v0.2.0 deleted it immediately on macOS and Linux.)
- Nothing is renamed over the running program unless its leading bytes say it is a program this
  machine can run.

## [0.2.0]

### Added

- The title bar can be dragged by clicking and holding anywhere on it, on Windows and Linux.
- The catalogue is laid out as a grid of cards, grouped into what is installed and what is available.
- You can choose where apps are installed, and move apps already installed to a new location.

### Fixed

- Removing an app installed from a DMG deleted the wrong directory instead of the app bundle itself.
- A DMG install's staging directory assumed the install location was always the default one.

### Changed

- "Verify" is temporarily hidden from a card's menu; it checks only one file of an install, which
  is not yet accurate for every platform. The command line's `verify` is unaffected.

## [0.1.0]

### Added

- First release.
