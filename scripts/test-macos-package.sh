#!/bin/bash
# Contract test using a real built agent-hub, without launching it or touching user data.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
[[ -f "$root/scripts/package-macos.sh" ]] || fail 'macOS packager is missing'
[[ $# == 2 ]] || fail 'usage: bash scripts/test-macos-package.sh BINARY VERSION'
binary=$1
version=$2
[[ -f "$binary" && -x "$binary" ]] || fail 'provide a real built executable'
work=$(mktemp -d "${TMPDIR:-/tmp}/agent-hub-package-test.XXXXXX")
trap 'rm -rf "$work"' EXIT

# Spaces in paths are deliberate. Packaging must copy, not wrap or rewrite the binary:
# current_exe(), self-replace and restart all need Contents/MacOS/agent-hub.
app="$work/with spaces/agent-hub.app"
bash "$root/scripts/package-macos.sh" "$binary" "$version" "$app"
plist="$app/Contents/Info.plist"
/usr/bin/plutil -lint "$plist"
[[ $(/usr/bin/plutil -extract CFBundleExecutable raw "$plist") == agent-hub ]]
[[ $(/usr/bin/plutil -extract CFBundlePackageType raw "$plist") == APPL ]]
[[ $(/usr/bin/plutil -extract CFBundleIdentifier raw "$plist") == io.github.aprazdnikov.agent-hub ]]
[[ $(/usr/bin/plutil -extract CFBundleShortVersionString raw "$plist") == "$version" ]]
[[ $(/usr/bin/plutil -extract CFBundleVersion raw "$plist") == "$version" ]]
[[ $(/usr/bin/plutil -extract CFBundleIconFile raw "$plist") == agent-hub.icns ]]
[[ -x "$app/Contents/MacOS/agent-hub" && ! -L "$app/Contents/MacOS/agent-hub" ]]
cmp "$binary" "$app/Contents/MacOS/agent-hub"
[[ ! -e "$app/Contents/_CodeSignature" ]]
/usr/bin/file "$app/Contents/MacOS/agent-hub"
/usr/bin/iconutil -c iconset "$app/Contents/Resources/agent-hub.icns" -o "$work/roundtrip.iconset"
[[ -f "$work/roundtrip.iconset/icon_512x512@2x.png" ]]
/usr/bin/sips -g pixelWidth -g pixelHeight "$work/roundtrip.iconset/icon_512x512@2x.png"
[[ $(/usr/bin/sips -g pixelWidth "$work/roundtrip.iconset/icon_512x512@2x.png") == *'pixelWidth: 1024'* ]]
[[ $(/usr/bin/sips -g pixelHeight "$work/roundtrip.iconset/icon_512x512@2x.png") == *'pixelHeight: 1024'* ]]

# Same archive format and top-level directory as the release workflow.
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$app" "$work/agent-hub.app.zip"
/usr/bin/ditto -x -k "$work/agent-hub.app.zip" "$work/unpacked"
unpacked="$work/unpacked/agent-hub.app"
[[ -x "$unpacked/Contents/MacOS/agent-hub" ]]
cmp "$binary" "$unpacked/Contents/MacOS/agent-hub"
cmp "$plist" "$unpacked/Contents/Info.plist"
/usr/bin/plutil -lint "$unpacked/Contents/Info.plist"

# Rejected input must not overwrite an existing installation or leave a partial app.
reject() {
    if bash "$root/scripts/package-macos.sh" "$@" >"$work/rejected.log" 2>&1; then
        fail "unexpectedly accepted: $*"
    fi
}
reject "$binary" "$version" "$app"
cmp "$binary" "$app/Contents/MacOS/agent-hub"
cmp "$plist" "$unpacked/Contents/Info.plist"
reject "$work/missing" "$version" "$work/missing.app"
[[ ! -e "$work/missing.app" ]]
reject "$binary" '1.2.3</string>' "$work/invalid-version.app"
[[ ! -e "$work/invalid-version.app" ]]
reject "$binary" "$version" "$work/not-a-bundle"
[[ ! -e "$work/not-a-bundle" ]]
reject "$root/scripts/make-icon.py" "$version" "$work/not-executable.app"
[[ ! -e "$work/not-executable.app" ]]
ln -s "$work/absent.app" "$work/symlink.app"
reject "$binary" "$version" "$work/symlink.app"
[[ -L "$work/symlink.app" && ! -e "$work/absent.app" ]]
printf 'PASS: bundle, plist, icon roundtrip, executable identity and ZIP roundtrip\n'
printf 'PASS: invalid inputs and existing/symlink output rejected without mutation\n'
