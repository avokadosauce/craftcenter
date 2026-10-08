#!/usr/bin/env bash
# shellcheck shell=bash
#
# Exercises packaging/linux/AppRun's desktop-integration logic without
# needing an actual AppImage runtime. It builds a throwaway AppDir, points
# HOME/XDG_DATA_HOME/APPIMAGE at a scratch directory, and checks the
# resulting desktop entry (or deliberate absence of one). Every scratch
# directory this script creates is removed on exit, including on failure,
# because /tmp on the box that runs this is memory-backed.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_run="$script_dir/AppRun"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0

fail() {
	echo "FAIL: $1" >&2
	failures=$((failures + 1))
}

pass() {
	echo "ok: $1"
}

# make_appdir DIR
#
# Lays out a minimal AppDir: AppRun (the real script under test), a stub
# desktop entry, and a stub binary that just echoes its invocation so we
# can tell it actually launched.
make_appdir() {
	local dir="$1"
	mkdir -p "$dir/usr/bin"
	cp "$app_run" "$dir/AppRun"
	chmod +x "$dir/AppRun"
	cat >"$dir/io.github.avokadosauce.craftcenter.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=CraftCenter
Exec=craftcenter
TryExec=craftcenter
Icon=io.github.avokadosauce.craftcenter
Categories=Utility;PackageManager;
EOF
	cat >"$dir/usr/bin/craftcenter" <<'EOF'
#!/bin/sh
echo "craftcenter stub launched: $*"
EOF
	chmod +x "$dir/usr/bin/craftcenter"
}

### Case 1: a normal run installs the desktop entry with Exec rewritten
### and TryExec removed.
case1_dir="$work/case1"
mkdir -p "$case1_dir"
appdir1="$case1_dir/AppDir"
make_appdir "$appdir1"
home1="$case1_dir/home"
data1="$case1_dir/home/.local/share"
mkdir -p "$home1"
fake_appimage="$case1_dir/CraftCenter-x86_64.AppImage"
: >"$fake_appimage"

desktop_dest="$data1/applications/io.github.avokadosauce.craftcenter.desktop"

output1="$(env -i HOME="$home1" XDG_DATA_HOME="$data1" APPIMAGE="$fake_appimage" PATH="$PATH" "$appdir1/AppRun" --flag 2>&1)"

if [ -f "$desktop_dest" ]; then
	pass "desktop entry installed on first run"
else
	fail "desktop entry was not installed at $desktop_dest"
fi

if grep -qF "Exec=$fake_appimage" "$desktop_dest" 2>/dev/null; then
	pass "Exec= rewritten to the AppImage path"
else
	fail "Exec= was not rewritten to the AppImage path"
fi

if grep -q '^TryExec=' "$desktop_dest" 2>/dev/null; then
	fail "TryExec= should have been removed but is still present"
else
	pass "TryExec= removed"
fi

if echo "$output1" | grep -qF "craftcenter stub launched: --flag"; then
	pass "the real binary was launched with its arguments"
else
	fail "the real binary does not appear to have launched (output: $output1)"
fi

### Case 2: a second run with no change to the source does not rewrite
### the installed file. Rather than racing a filesystem's mtime
### resolution with a sleep, force the mtime backwards to a known value
### first: if AppRun rewrites the file, the mtime will visibly move
### forward from that fixed point; if it does not, it stays put.
touch -d '2000-01-01T00:00:00' "$desktop_dest"
mtime_before="$(stat -c %Y "$desktop_dest" 2>/dev/null || stat -f %m "$desktop_dest")"
env -i HOME="$home1" XDG_DATA_HOME="$data1" APPIMAGE="$fake_appimage" PATH="$PATH" "$appdir1/AppRun" >/dev/null 2>&1
mtime_after="$(stat -c %Y "$desktop_dest" 2>/dev/null || stat -f %m "$desktop_dest")"
if [ "$mtime_before" = "$mtime_after" ]; then
	pass "repeated run did not rewrite an unchanged desktop entry"
else
	fail "repeated run rewrote the desktop entry even though nothing changed"
fi

### Case 3: CRAFTCENTER_NO_DESKTOP_INTEGRATION=1 suppresses installation
### entirely, for a fresh HOME that has never seen an entry.
case3_dir="$work/case3"
mkdir -p "$case3_dir"
appdir3="$case3_dir/AppDir"
make_appdir "$appdir3"
home3="$case3_dir/home"
data3="$case3_dir/home/.local/share"
mkdir -p "$home3"
fake_appimage3="$case3_dir/CraftCenter-x86_64.AppImage"
: >"$fake_appimage3"
desktop_dest3="$data3/applications/io.github.avokadosauce.craftcenter.desktop"

output3="$(env -i HOME="$home3" XDG_DATA_HOME="$data3" APPIMAGE="$fake_appimage3" \
	CRAFTCENTER_NO_DESKTOP_INTEGRATION=1 PATH="$PATH" "$appdir3/AppRun" 2>&1)"

if [ -f "$desktop_dest3" ]; then
	fail "desktop entry was installed despite CRAFTCENTER_NO_DESKTOP_INTEGRATION=1"
else
	pass "CRAFTCENTER_NO_DESKTOP_INTEGRATION=1 suppressed installation"
fi

if echo "$output3" | grep -qF "craftcenter stub launched:"; then
	pass "launch still happens with the opt-out set"
else
	fail "the opt-out should not have prevented the program from launching"
fi

### Case 4: a hostile AppImage path is refused, but the program still
### launches. Tried with both a '%' and a '"' in the path, since either
### alone is enough to break a generated Exec= line.
for hostile_fragment in '%n' '"quoted"'; do
	case4_dir="$work/case4-$RANDOM"
	mkdir -p "$case4_dir"
	appdir4="$case4_dir/AppDir"
	make_appdir "$appdir4"
	home4="$case4_dir/home"
	data4="$case4_dir/home/.local/share"
	mkdir -p "$home4"
	hostile_appimage="$case4_dir/CraftCenter-${hostile_fragment}.AppImage"
	: >"$hostile_appimage"
	desktop_dest4="$data4/applications/io.github.avokadosauce.craftcenter.desktop"

	output4="$(env -i HOME="$home4" XDG_DATA_HOME="$data4" APPIMAGE="$hostile_appimage" \
		PATH="$PATH" "$appdir4/AppRun" 2>&1)"

	if [ -f "$desktop_dest4" ]; then
		fail "a hostile path ('$hostile_fragment') was still written into a desktop entry"
	else
		pass "hostile path ('$hostile_fragment') was refused"
	fi

	if echo "$output4" | grep -qF "craftcenter stub launched:"; then
		pass "launch still happens for a hostile path ('$hostile_fragment')"
	else
		fail "launch did not happen for a hostile path ('$hostile_fragment')"
	fi
done

if [ "$failures" -eq 0 ]; then
	echo "apprun-test.sh: all checks passed"
	exit 0
else
	echo "apprun-test.sh: $failures check(s) failed"
	exit 1
fi
