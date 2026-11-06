#!/bin/bash
# Package an existing Linux x86-64 executable without installing it.
set -euo pipefail
export LC_ALL=C
fail() { printf 'package-deb: %s\n' "$*" >&2; exit 1; }
[[ $# == 3 ]] || fail 'usage: bash scripts/package-deb.sh BINARY VERSION OUTPUT.deb'
binary=$1
version=$2
output=$3
[[ -f "$binary" && -x "$binary" ]] || fail 'BINARY must be an executable file'
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'VERSION must be X.Y.Z'
[[ $output == *.deb ]] || fail 'output must end in .deb'
[[ ! -e "$output" && ! -L "$output" ]] || fail 'output already exists; refusing to replace it'
for command in dpkg-deb dpkg-shlibdeps readelf python3; do
    command -v "$command" >/dev/null || fail "$command is required"
done
header=$(readelf -h "$binary" 2>/dev/null) || fail 'BINARY must be an ELF executable'
[[ $header == *'Class:                             ELF64'* && $header == *'Machine:                           Advanced Micro Devices X86-64'* ]] || fail 'BINARY must be an x86-64 ELF executable'
[[ $header == *'Type:                              EXEC'* || $header == *'Type:                              DYN'* ]] || fail 'BINARY must be an ELF executable'
readelf -l "$binary" | grep -q 'Requesting program interpreter' || fail 'BINARY must be a dynamically linked executable'
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$(dirname "$output")"
work=$(mktemp -d "$(dirname "$output")/.agent-hub-deb.XXXXXX")
trap 'rm -rf "$work"' EXIT
work=$(cd "$work" && pwd)
stage="$work/debian/agent-hub"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/agent-hub" \
    "$stage/usr/share/applications" "$stage/usr/share/icons/hicolor/256x256/apps" \
    "$stage/usr/share/doc/agent-hub" "$work/debian"
install -m 755 "$binary" "$stage/usr/bin/agent-hub"
install -m 644 "$root/README.md" "$stage/usr/share/doc/agent-hub/README.md"
install -m 644 "$root/LICENSE" "$stage/usr/share/doc/agent-hub/copyright"
printf 'deb\n' > "$stage/usr/share/agent-hub/install-kind"
cat > "$stage/usr/share/applications/agent-hub.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Agent Hub
Comment=Manage AI agent sessions
Exec=/usr/bin/agent-hub
Icon=agent-hub
Terminal=false
Categories=Development;Utility;
DESKTOP
python3 - "$root/scripts/make-icon.py" "$stage/usr/share/icons/hicolor/256x256/apps/agent-hub.png" <<'PY'
import runpy
import sys
from pathlib import Path
Path(sys.argv[2]).write_bytes(runpy.run_path(sys.argv[1])["png"](256))
PY
chmod 644 "$stage/usr/share/agent-hub/install-kind" "$stage/usr/share/applications/agent-hub.desktop" "$stage/usr/share/icons/hicolor/256x256/apps/agent-hub.png"
# dpkg-shlibdeps calculates minimum versions from the supplied executable and the
# builder's package database. It does not discover dynamically loaded libraries.
printf 'Source: agent-hub\nSection: utils\nPriority: optional\nMaintainer: agent-hub contributors <noreply@github.com>\n\nPackage: agent-hub\nArchitecture: amd64\nDescription: AI agent session manager\n' > "$work/debian/control"
depends=$(cd "$work" && dpkg-shlibdeps -O -e debian/agent-hub/usr/bin/agent-hub)
[[ $depends == 'shlibs:Depends='* && $depends == *libc6* ]] || fail 'could not determine shared-library dependencies'
depends=${depends#shlibs:Depends=}
# winit, glutin and their X11/Wayland backends load these at runtime.
for dependency in libegl1 libgl1 libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 libxrender1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-cursor0 libwayland-egl1; do
    if [[ ", $depends," != *", $dependency,"* && ", $depends," != *", $dependency ("* ]]; then
        depends+=", $dependency"
    fi
done
cat > "$stage/DEBIAN/control" <<CONTROL
Package: agent-hub
Version: $version
Architecture: amd64
Maintainer: agent-hub contributors <noreply@github.com>
Section: utils
Priority: optional
Depends: $depends
Homepage: https://github.com/aprazdnikov/agent-hub-rs
Description: AI agent session manager
 Desktop application for managing AI agent sessions.
CONTROL
chmod 644 "$stage/DEBIAN/control"
find "$stage" -type d -exec chmod 755 {} +
dpkg-deb --root-owner-group --build "$stage" "$work/result.deb"
# A hard link atomically refuses an output created after the initial check.
ln -T "$work/result.deb" "$output" || fail 'could not create output without replacing an existing file'
printf 'Created %s\n' "$output"
