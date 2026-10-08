#!/usr/bin/env bash
# shellcheck shell=bash
#
# Builds CraftCenter and craftcenter-cli for both Apple Silicon and Intel,
# lipos them into universal binaries, assembles CraftCenter.app, signs it
# (ad-hoc by default, or with a real identity / notarization when the
# relevant secrets are present), and produces the two macOS release
# assets: craftcenter-<version>-macos-universal.dmg and
# craftcenter-cli-<version>-macos-universal.zip.
#
# This script only runs correctly on macOS (it needs lipo, hdiutil,
# ditto, codesign and friends), and is not run as part of this review.
set -euo pipefail

# shellcheck source=../env.sh
. "$(dirname "$0")/../env.sh"

APP_NAME="CraftCenter"
# The bundle identifier lives in Info.plist.in and nowhere else, so there is one place to change it.
SIGN_IDENTITY="${MACOS_SIGN_IDENTITY:--}"

mkdir -p "$DIST" "$CARGO_TARGET_DIR"

WORK="$(mktemp -d "$CARGO_TARGET_DIR/macos-package.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

TARGETS="aarch64-apple-darwin x86_64-apple-darwin"
for target in $TARGETS; do
	( cd "$ROOT" && cargo build --release --locked --target "$target" -p craftcenter -p craftcenter-cli )
done

lipo_pair() {
	local bin_name="$1" out="$2"
	lipo -create -output "$out" \
		"$CARGO_TARGET_DIR/aarch64-apple-darwin/release/$bin_name" \
		"$CARGO_TARGET_DIR/x86_64-apple-darwin/release/$bin_name"
}

UNIVERSAL_CRAFTCENTER="$WORK/craftcenter-universal"
UNIVERSAL_CLI="$WORK/craftcenter-cli-universal"
lipo_pair craftcenter "$UNIVERSAL_CRAFTCENTER"
lipo_pair craftcenter-cli "$UNIVERSAL_CLI"
chmod +x "$UNIVERSAL_CRAFTCENTER" "$UNIVERSAL_CLI"

### Assemble CraftCenter.app
APP="$WORK/$APP_NAME.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$UNIVERSAL_CRAFTCENTER" "$APP/Contents/MacOS/craftcenter"
chmod +x "$APP/Contents/MacOS/craftcenter"

sed \
	-e "s/@VERSION@/$VERSION/g" \
	-e "s/@BUILD@/$CRAFTCENTER_BUILD_SHA/g" \
	"$ROOT/packaging/macos/Info.plist.in" \
	>"$APP/Contents/Info.plist"

ICON_SRC="$ROOT/assets/app-icon/craftcenter.icns"
if [ -f "$ICON_SRC" ]; then
	cp "$ICON_SRC" "$APP/Contents/Resources/craftcenter.icns"
else
	warn "no assets/app-icon/craftcenter.icns found; CraftCenter.app will use the default icon"
fi

### Signing. Ad-hoc signing (the "-" identity) works everywhere and lets
### the app run locally; a real identity is only available when the
### MACOS_SIGN_IDENTITY secret has been configured, and its absence must
### never fail the build, only degrade to ad-hoc.
codesign --force --deep --sign "$SIGN_IDENTITY" "$APP"

### Notarization + stapling, only when every required Apple credential is
### present. A partially-configured set (e.g. APPLE_ID without
### APPLE_PASSWORD) is treated the same as none at all: warn and ship an
### ad-hoc or identity-signed build without Apple's notarization ticket.
if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
	NOTARIZE_ZIP="$WORK/notarize-submission.zip"
	ditto -c -k --keepParent "$APP" "$NOTARIZE_ZIP"
	xcrun notarytool submit --wait \
		--apple-id "$APPLE_ID" \
		--password "$APPLE_PASSWORD" \
		--team-id "$APPLE_TEAM_ID" \
		"$NOTARIZE_ZIP"
	xcrun stapler staple "$APP"
else
	warn "APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID are not all set; shipping without notarization"
fi

### .dmg: a small HFS+ volume holding the .app and a symlink to
### /Applications, so dragging the icon across is the entire install.
DMG_ROOT="$WORK/dmg-root"
mkdir -p "$DMG_ROOT"
cp -R "$APP" "$DMG_ROOT/"
ln -s /Applications "$DMG_ROOT/Applications"

RAW_DMG="$WORK/craftcenter-raw.dmg"
FINAL_DMG="$DIST/craftcenter-$VERSION-macos-universal.dmg"
hdiutil makehybrid -hfs -hfs-volume-name "$APP_NAME" -o "$RAW_DMG" "$DMG_ROOT"
hdiutil convert "$RAW_DMG" -format UDZO -o "$FINAL_DMG"

### CLI zip: the headless binary plus the same top-level docs the other
### platforms carry, wrapped in a named directory so unzipping does not
### scatter files into whatever folder the user unzips into.
CLI_STAGE_NAME="craftcenter-cli-$VERSION-macos-universal"
CLI_STAGE="$WORK/$CLI_STAGE_NAME"
mkdir -p "$CLI_STAGE"
cp "$UNIVERSAL_CLI" "$CLI_STAGE/craftcenter-cli"
chmod +x "$CLI_STAGE/craftcenter-cli"
copy_docs "$CLI_STAGE"

( cd "$WORK" && ditto -c -k --keepParent "$CLI_STAGE_NAME" "$DIST/$CLI_STAGE_NAME.zip" )

echo "packaging/macos/package.sh: done, artifacts in $DIST"
