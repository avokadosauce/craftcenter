# Roadmap

Honest status first: this is a first release of a program whose window has not yet been opened by a
person. See `README.md` for exactly which steps have been run and which have not.

## Now

- **A person runs the window on each platform.** The parts most likely to need adjusting are the
  app-drawn title bar (drag, double-click to maximize, the caption buttons) and the per-platform
  install paths. `CRAFTCENTER_OS_DECORATIONS=1` exists as a way out if the title bar misbehaves.
- **macOS and Windows installs.** The DMG mount-and-copy and the portable-zip paths are written and
  type-checked on their own platforms in CI, but no person has installed an app with them yet.

## Next

- **Delta updates on Linux.** `photocraft` publishes an AppImage `.zsync` index; nothing else does
  yet. Implementing zsync's rolling checksums would turn a 60 MB update into a few megabytes for
  the apps that have one. The format table already records which releases carry it.
- **A system-wide install mode**, behind an explicit choice, using the upstream `.deb`/`.rpm`/`.msi`
  and the elevation they require. The per-user path stays the default.
- **A Flatpak of CraftCenter itself**, and of the apps that publish one (`photocraft`, `deckcraft`
  today).
- **Showing download sizes before installing**, from the release metadata rather than a `HEAD`
  request per asset.

## Not planned

- **A web build.** Every Crafting App ships one; CraftCenter will not. A page in a browser cannot
  install a desktop application, and shipping one would only invite the question.
- **Telemetry, analytics or crash reporting**, in any form.
- **A credential prompt.** The update check costs no API rate limit, so there is nothing a token
  would buy.
- **Installing `artcraft`, the engine.** It is not one of the creative-suite apps, its releases
  have a different shape, and it publishes no checksum manifest, so a download could not be
  verified. It is deliberately absent from the catalogue.
