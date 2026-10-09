#!/bin/bash
# Build a bundle around the unchanged release executable. Requires macOS and Python 3.
set -euo pipefail

fail() { printf 'package-macos: %s\n' "$*" >&2; exit 1; }
[[ $# == 3 ]] || fail 'usage: bash scripts/package-macos.sh BINARY VERSION OUTPUT.app'
[[ $(uname -s) == Darwin ]] || fail 'macOS is required (plutil and iconutil)'
binary=$1
version=$2
output=$3
[[ -f "$binary" && -x "$binary" ]] || fail 'BINARY must be an executable file'
[[ $(/usr/bin/file -b "$binary") == *Mach-O*executable* ]] || fail 'BINARY must be a Mach-O executable'
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'VERSION must be X.Y.Z'
[[ $output == *.app ]] || fail 'output must end in .app'
[[ ! -e "$output" && ! -L "$output" ]] || fail 'output already exists; refusing to replace it'
root=$(cd "$(dirname "$0")/.." && pwd)

# Build off to the side so failed icon/plist generation leaves no partial app.
work=$(mktemp -d "${TMPDIR:-/tmp}/agent-hub-package.XXXXXX")
trap 'rm -rf "$work"' EXIT
app="$work/agent-hub.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/agent-hub"
chmod 755 "$app/Contents/MacOS/agent-hub"
python3 "$root/scripts/make-icon.py" --iconset "$work/agent-hub.iconset"
/usr/bin/iconutil -c icns "$work/agent-hub.iconset" -o "$app/Contents/Resources/agent-hub.icns"

plist="$app/Contents/Info.plist"
/usr/bin/plutil -create xml1 "$plist"
/usr/bin/plutil -insert CFBundleDevelopmentRegion -string en "$plist"
/usr/bin/plutil -insert CFBundleIdentifier -string io.github.aprazdnikov.agent-hub "$plist"
/usr/bin/plutil -insert CFBundleName -string agent-hub "$plist"
/usr/bin/plutil -insert CFBundleDisplayName -string agent-hub "$plist"
/usr/bin/plutil -insert CFBundleExecutable -string agent-hub "$plist"
/usr/bin/plutil -insert CFBundlePackageType -string APPL "$plist"
/usr/bin/plutil -insert CFBundleInfoDictionaryVersion -string 6.0 "$plist"
/usr/bin/plutil -insert CFBundleShortVersionString -string "$version" "$plist"
/usr/bin/plutil -insert CFBundleVersion -string "$version" "$plist"
/usr/bin/plutil -insert CFBundleIconFile -string agent-hub.icns "$plist"
/usr/bin/plutil -insert NSHighResolutionCapable -bool YES "$plist"
/usr/bin/plutil -lint "$plist"

# No launcher or bundle signing: current_exe/self-replace must still target this binary.
# The linker-provided signature (if any) is preserved byte-for-byte with the executable.
mkdir -p "$(dirname "$output")"
mv "$app" "$output"
printf 'Created %s\n' "$output"
