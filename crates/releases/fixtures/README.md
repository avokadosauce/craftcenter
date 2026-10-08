# Recorded releases

What each app's latest release looked like when these files were recorded, so the test suite runs
with no network and CI never depends on GitHub being reachable — or on the upstream not shipping.

- `<app>/redirect.txt` — the `location` header that
  `https://github.com/storytold/<app>/releases/latest/download/SHA256SUMS.txt` answered with. The
  release tag is read out of it.
- `<app>/SHA256SUMS.txt` — that release's checksum manifest: every asset name, with its SHA-256.
- `<app>/latest.json` — the REST API's view of the same release, for the fallback parser. Recorded
  for two apps; the fallback runs only when a repository publishes no manifest.

**These are a dated snapshot, deliberately.** Recorded 2026-10-08. The upstream ships quickly —
on the day these were taken, `photocraft` went from v0.3.0 to v0.5.0 within the hour and
`soundcraft` published its first release twenty-five seconds after it was recorded as having none.
Refreshing them would not keep them current for long, and two of the cases they capture are
exactly the ones the selection tests exist for:

- **`pdfcraft` published under the `printcraft-*` stem.** The repository is `pdfcraft`, its binary
  and app id are `pdfcraft`, and every release up to v0.2.1 was named `printcraft-*`. (Its v0.4.0
  release has since flipped to `pdfcraft-*`; the catalogue row lists both stems, newest first, so
  both resolve.) This is why asset selection matches a release's real asset list and never builds a
  filename from a template.
- **`soundcraft` had no release at all.** Its `redirect.txt` records `no-release`, the site having
  answered `404`, and it has no manifest. An app with no release is a state this program renders,
  not a failure, and keeping the recording keeps that path honest.

To see what the live releases look like now, and what has drifted from what the catalogue expects:

```sh
cargo xtask catalogue --check
```

That one needs the network and is not run in CI, because an upstream project shipping a release
must not turn this repository red.
