# Security Policy

CraftCenter downloads software from the network and installs it on the machine it runs on. That
makes release-asset integrity, archive handling and the install paths the parts worth attacking,
and the parts worth reporting on.

## Reporting a vulnerability

Please give the maintainers a reasonable opportunity to investigate and fix a problem before
publishing exploitable details or proof-of-concept code.

This repository does not yet document a private security contact. Open an issue with the smallest
non-exploitable description that lets a maintainer make contact, and ask for a private channel
before sharing anything weaponised. Do not open a public issue containing a working exploit, a
credential, or a malicious archive.

## What is in scope

- **Release-asset integrity**: the checksum manifest parser, the digest comparison, anything that
  could cause an unverified or substituted asset to be installed.
- **Host handling**: the allow-list in `crates/releases/src/http.rs`, redirect following, and any
  path by which a request could be made to a host other than GitHub.
- **Archive handling**: tar and zip extraction, entry names that escape the destination directory,
  symlink and path-traversal handling, decompression bombs.
- **Install and removal**: writes outside the user's own directories, the atomic version swap, the
  self-replacement of the running program, the state file.
- **The desktop entry and shortcut writers**: injection through a path or an app name.
- **Supply chain**: the dependency set, CI, and the release workflow.

## What this program does not defend against

Stated plainly, because a security policy that overclaims is worse than none.

- **The upstream's `SHA256SUMS.txt` is unsigned.** No upstream repository publishes a detached
  signature, minisign or cosign bundle beside it. The trust root is therefore TLS plus GitHub's
  control of the release. A verified digest proves the bytes are the ones that release published;
  it does not prove who published them. Where a platform signature exists — a notarised, stapled
  DMG; an Authenticode signature — CraftCenter checks that as well, and that check *is* a publisher
  identity check.
- **An attacker who controls the GitHub repository** controls both the assets and the manifest, and
  no checksum helps.
- **CraftCenter does not sandbox what it installs.** The apps run with the user's own rights, as
  they would if the user had downloaded them by hand.

## Reports that are not security issues

A download that fails, a release that is missing a platform's asset, an app that does not start
after installation — those are ordinary bugs. Please open a normal issue.
