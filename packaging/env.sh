# shellcheck shell=bash
#
# Shared environment for the packaging scripts. Source this near the top of
# every packaging/<os>/*.sh with:
#
#   . "$(dirname "$0")/../env.sh"
#
# It resolves paths relative to its own location (not to the caller's $0),
# so it behaves the same whether it is sourced directly or through a
# relative path assembled by another script.
#
# Exports: ROOT, VERSION, DIST, CRAFTCENTER_BUILD_SHA, CRAFTCENTER_BUILD_DATE,
# CARGO_TARGET_DIR. Defines: warn(), copy_docs(), sha256().

set -euo pipefail

# BASH_SOURCE[0] is this file, regardless of how deep the sourcing chain is.
_craftcenter_env_sh_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${_craftcenter_env_sh_dir}/.." && pwd)"
unset _craftcenter_env_sh_dir
export ROOT

# warn MESSAGE
#
# Reports a non-fatal problem. Under GitHub Actions this surfaces as an
# annotation on the job summary; everywhere else it is a plain line on
# stderr so it does not get mixed into anything a caller might capture from
# stdout.
warn() {
	local message="$1"
	if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
		printf '::warning::%s\n' "$message"
	else
		printf 'warning: %s\n' "$message" >&2
	fi
}

# The version is the single source of truth for every asset name, so it is
# read once here rather than re-parsed by each packaging script. Reading it
# with awk (rather than a TOML parser we would otherwise have to vendor)
# keeps this script dependency-free; it only has to understand enough TOML
# to find the first `version = "..."` line inside `[workspace.package]`.
if [ -z "${CRAFTCENTER_VERSION:-}" ]; then
	CRAFTCENTER_VERSION="$(awk '
		/^\[workspace\.package\]/ { in_section = 1; next }
		/^\[/ { in_section = 0 }
		in_section && /^version[[:space:]]*=/ {
			if (match($0, /"[^"]*"/)) {
				print substr($0, RSTART + 1, RLENGTH - 2)
				exit
			}
		}
	' "$ROOT/Cargo.toml")"
fi
if [ -z "$CRAFTCENTER_VERSION" ]; then
	echo "env.sh: could not read [workspace.package] version from $ROOT/Cargo.toml" \
		"(set CRAFTCENTER_VERSION to override)" >&2
	exit 1
fi
VERSION="$CRAFTCENTER_VERSION"
export VERSION

DIST="${DIST:-$ROOT/dist/release}"
export DIST

if [ -z "${CRAFTCENTER_BUILD_SHA:-}" ]; then
	CRAFTCENTER_BUILD_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
fi
export CRAFTCENTER_BUILD_SHA

CRAFTCENTER_BUILD_DATE="${CRAFTCENTER_BUILD_DATE:-$(date -u +%Y-%m-%d)}"
export CRAFTCENTER_BUILD_DATE

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export CARGO_TARGET_DIR

# copy_docs DEST_DIR
#
# Copies the project's top-level legal and readme files into DEST_DIR, for
# scripts assembling a package layout that should carry them along (e.g.
# usr/share/doc/craftcenter). Each file is optional: a repo that has not
# yet grown a NOTICE or ATTRIBUTION.md still packages cleanly.
copy_docs() {
	local dest="$1"
	mkdir -p "$dest"
	local name
	for name in README.md LICENSE.md NOTICE ATTRIBUTION.md; do
		if [ -f "$ROOT/$name" ]; then
			cp "$ROOT/$name" "$dest/$name"
		fi
	done
}

# sha256 FILE
#
# Prints just the hex digest (no filename, no trailing text) so callers can
# use it inline, e.g. in a generated metadata file.
sha256() {
	local file="$1"
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum -- "$file" | awk '{ print $1 }'
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 -- "$file" | awk '{ print $1 }'
	else
		echo "env.sh: sha256 needs sha256sum or shasum, neither is on PATH" >&2
		exit 1
	fi
}
