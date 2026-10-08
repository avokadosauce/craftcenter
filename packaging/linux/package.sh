#!/usr/bin/env bash
# shellcheck shell=bash
#
# Builds and packages CraftCenter for the host's Linux architecture:
# a relocatable .tar.gz and a self-updating .AppImage, each named
# craftcenter-<version>-linux-<arch>.<ext> to match the asset naming the
# other Crafting Apps already use.
#
# Usage: packaging/linux/package.sh [--formats tar,appimage] [--skip-build]
set -euo pipefail

# shellcheck source=../env.sh
. "$(dirname "$0")/../env.sh"

APP_ID="io.github.avokadosauce.craftcenter"
FORMATS="${FORMATS:-tar appimage}"
SKIP_BUILD=0

while [ $# -gt 0 ]; do
	case "$1" in
	--formats)
		FORMATS="$(printf '%s' "$2" | tr ',' ' ')"
		shift 2
		;;
	--formats=*)
		FORMATS="$(printf '%s' "${1#--formats=}" | tr ',' ' ')"
		shift
		;;
	--skip-build)
		SKIP_BUILD=1
		shift
		;;
	*)
		echo "packaging/linux/package.sh: unknown argument: $1" >&2
		exit 2
		;;
	esac
done

case "$(uname -m)" in
x86_64) ARCH=x86_64 ;;
aarch64) ARCH=aarch64 ;;
arm64) ARCH=aarch64 ;;
*)
	echo "packaging/linux/package.sh: unsupported architecture $(uname -m)" >&2
	exit 1
	;;
esac

mkdir -p "$DIST" "$CARGO_TARGET_DIR"

# A scratch directory for assembling package contents. It lives under
# CARGO_TARGET_DIR rather than /tmp: this box treats /tmp as RAM, and the
# intermediate tree here is sizeable (both binaries plus docs and icons).
WORK="$(mktemp -d "$CARGO_TARGET_DIR/linux-package.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [ "$SKIP_BUILD" -eq 0 ]; then
	( cd "$ROOT" && cargo build --release --locked -p craftcenter -p craftcenter-cli )
fi

RELEASE_DIR="$CARGO_TARGET_DIR/release"
BIN_CRAFTCENTER="$RELEASE_DIR/craftcenter"
BIN_CLI="$RELEASE_DIR/craftcenter-cli"

for bin in "$BIN_CRAFTCENTER" "$BIN_CLI"; do
	if [ ! -x "$bin" ]; then
		echo "packaging/linux/package.sh: expected build output missing: $bin" \
			"(pass --skip-build only when it already exists)" >&2
		exit 1
	fi
done

# Stage the FHS tree once; both the tarball and the AppImage are built from
# the same usr/ contents so there is exactly one place that decides what
# ships.
STAGE="$WORK/usr-stage"
mkdir -p \
	"$STAGE/usr/bin" \
	"$STAGE/usr/share/applications" \
	"$STAGE/usr/share/metainfo" \
	"$STAGE/usr/share/doc/craftcenter"

cp "$BIN_CRAFTCENTER" "$STAGE/usr/bin/craftcenter"
cp "$BIN_CLI" "$STAGE/usr/bin/craftcenter-cli"

cp "$ROOT/packaging/linux/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"

sed \
	-e "s/@VERSION@/$VERSION/g" \
	-e "s/@DATE@/$CRAFTCENTER_BUILD_DATE/g" \
	"$ROOT/packaging/linux/$APP_ID.metainfo.xml.in" \
	>"$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"

copy_docs "$STAGE/usr/share/doc/craftcenter"

# Icons are named assets/app-icon/craftcenter-<size>.png, one file per
# hicolor size, mirroring how the per-app icons already in that directory
# are named (e.g. photocraft-64.png). None is required: a repo that has not
# yet grown a craftcenter icon still packages, just without one.
icon_found=0
for icon_src in "$ROOT"/assets/app-icon/craftcenter-*.png; do
	[ -e "$icon_src" ] || continue
	size="$(basename "$icon_src" .png)"
	size="${size#craftcenter-}"
	case "$size" in
	'' | *[!0-9]*) continue ;;
	esac
	icon_dir="$STAGE/usr/share/icons/hicolor/${size}x${size}/apps"
	mkdir -p "$icon_dir"
	cp "$icon_src" "$icon_dir/$APP_ID.png"
	icon_found=1
done
if [ "$icon_found" -eq 0 ]; then
	warn "no assets/app-icon/craftcenter-<size>.png found; packaging without an application icon"
fi

if command -v desktop-file-validate >/dev/null 2>&1; then
	desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
else
	warn "desktop-file-validate not found; skipping desktop entry validation"
fi

if command -v appstreamcli >/dev/null 2>&1; then
	appstreamcli validate --no-net "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
else
	warn "appstreamcli not found; skipping AppStream metadata validation"
fi

build_tar() {
	local tar_name="craftcenter-$VERSION-linux-$ARCH"
	local tar_root="$WORK/tar/$tar_name"
	mkdir -p "$tar_root"
	# "Holding the contents of usr/" means bin/ and share/ sit directly
	# under the top-level directory, not nested under another usr/ -
	# matching the relocatable layout the other Crafting Apps ship.
	cp -a "$STAGE/usr/." "$tar_root/"
	tar -C "$WORK/tar" -czf "$DIST/$tar_name.tar.gz" "$tar_name"
}

build_appimage() {
	local appdir="$WORK/AppDir"
	mkdir -p "$appdir/usr"
	cp -a "$STAGE/usr/." "$appdir/usr/"

	install -m 0755 "$ROOT/packaging/linux/AppRun" "$appdir/AppRun"
	cp "$ROOT/packaging/linux/$APP_ID.desktop" "$appdir/$APP_ID.desktop"

	# appimagetool expects an icon file at the AppDir root matching the
	# desktop entry's Icon= key. Reuse the largest hicolor icon we staged,
	# if any; without one, appimagetool will warn but still builds.
	local root_icon=""
	local candidate
	for candidate in 512 256 128 64 48 32 16; do
		if [ -f "$appdir/usr/share/icons/hicolor/${candidate}x${candidate}/apps/$APP_ID.png" ]; then
			root_icon="$appdir/usr/share/icons/hicolor/${candidate}x${candidate}/apps/$APP_ID.png"
			break
		fi
	done
	if [ -n "$root_icon" ]; then
		cp "$root_icon" "$appdir/$APP_ID.png"
	fi

	local appimagetool="${APPIMAGETOOL:-}"
	if [ -z "$appimagetool" ]; then
		if command -v appimagetool >/dev/null 2>&1; then
			appimagetool="appimagetool"
		else
			appimagetool="$CARGO_TARGET_DIR/appimagetool-$ARCH.AppImage"
			if [ ! -x "$appimagetool" ]; then
				local url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
				echo "packaging/linux/package.sh: downloading appimagetool from $url"
				curl -fsSL -o "$appimagetool" "$url"
				chmod +x "$appimagetool"
			fi
		fi
	fi

	local out="$DIST/craftcenter-$VERSION-linux-$ARCH.AppImage"
	# APPIMAGE_EXTRACT_AND_RUN avoids needing FUSE to run appimagetool
	# itself, which matters on CI runners and minimal containers alike.
	# -u embeds zsync update information pointing at this project's GitHub
	# releases, so an already-installed AppImage can update itself in
	# place; --no-appstream skips appimagetool's own (duplicate) AppStream
	# validation step since we already ran appstreamcli above when present.
	APPIMAGE_EXTRACT_AND_RUN=1 "$appimagetool" \
		-u "gh-releases-zsync|avokadosauce|craftcenter|latest|craftcenter-*-linux-$ARCH.AppImage.zsync" \
		--no-appstream \
		"$appdir" "$out"

	if command -v zsyncmake >/dev/null 2>&1; then
		( cd "$DIST" && zsyncmake -u "$(basename "$out")" "$(basename "$out")" )
	else
		warn "zsyncmake not found; the AppImage was built without a .zsync file"
	fi
}

for fmt in $FORMATS; do
	case "$fmt" in
	tar) build_tar ;;
	appimage) build_appimage ;;
	*)
		echo "packaging/linux/package.sh: unknown format '$fmt' (expected 'tar' or 'appimage')" >&2
		exit 2
		;;
	esac
done

echo "packaging/linux/package.sh: done, artifacts in $DIST"
