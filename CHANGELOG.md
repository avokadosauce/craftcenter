# Changelog

All notable changes to CraftCenter are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
