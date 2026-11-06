#!/bin/bash
# Inspect a package in a temporary directory; never install or run it.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
[[ $# == 2 ]] || fail 'usage: bash scripts/test-deb-package.sh BINARY VERSION'
binary=$1
version=$2
work=$(mktemp -d "${TMPDIR:-/tmp}/agent-hub-deb-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
package="$work/with spaces/agent-hub.deb"
bash "$root/scripts/package-deb.sh" "$binary" "$version" "$package"
[[ $(dpkg-deb -f "$package" Package) == agent-hub ]]
[[ $(dpkg-deb -f "$package" Version) == "$version" ]]
[[ $(dpkg-deb -f "$package" Architecture) == amd64 ]]
depends=$(dpkg-deb -f "$package" Depends)
for dependency in libc6 libegl1 libgl1 libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 libxrender1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-cursor0 libwayland-egl1; do
    [[ ", $depends," == *", $dependency,"* || ", $depends," == *", $dependency ("* ]] || fail "missing dependency: $dependency"
done
dpkg-deb -x "$package" "$work/data"
dpkg-deb -e "$package" "$work/control"
base="$work/data/usr"
cmp "$binary" "$base/bin/agent-hub"
[[ $(stat -c %a "$base/bin/agent-hub") == 755 ]]
[[ $(cat "$base/share/agent-hub/install-kind") == deb ]]
[[ $(stat -c %a "$base/share/agent-hub/install-kind") == 644 ]]
[[ $(stat -c %a "$base/share/applications/agent-hub.desktop") == 644 ]]
[[ $(stat -c %a "$base/share/icons/hicolor/256x256/apps/agent-hub.png") == 644 ]]
python3 - "$base/share/icons/hicolor/256x256/apps/agent-hub.png" <<'PY'
import struct
import sys

with open(sys.argv[1], "rb") as image:
    header = image.read(24)
if len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
    raise SystemExit("FAIL: invalid PNG header")
if struct.unpack(">II", header[16:24]) != (256, 256):
    raise SystemExit("FAIL: unexpected icon dimensions")
PY
grep -Fxq 'Exec=/usr/bin/agent-hub' "$base/share/applications/agent-hub.desktop"
grep -Fxq 'Icon=agent-hub' "$base/share/applications/agent-hub.desktop"
cmp "$root/README.md" "$base/share/doc/agent-hub/README.md"
cmp "$root/LICENSE" "$base/share/doc/agent-hub/copyright"
[[ ! -e "$work/control/preinst" && ! -e "$work/control/postinst" && ! -e "$work/control/prerm" && ! -e "$work/control/postrm" ]]
# No package path may touch user data or install outside the declared prefix.
[[ $(find "$work/data" -type f | wc -l) == 6 ]]
[[ $(dpkg-deb --contents "$package" | awk '{print $2}' | sort -u) == root/root ]]
reject() {
    if bash "$root/scripts/package-deb.sh" "$@" >"$work/rejected.log" 2>&1; then
        fail "unexpectedly accepted: $*"
    fi
}
cp "$package" "$work/original.deb"
reject "$binary" "$version" "$package"
cmp "$package" "$work/original.deb"
reject "$work/missing" "$version" "$work/missing.deb"
[[ ! -e "$work/missing.deb" ]]
reject "$binary" $'1.2.3\nDepends: bad' "$work/invalid.deb"
[[ ! -e "$work/invalid.deb" ]]
reject "$binary" "$version" "$work/no-extension"
[[ ! -e "$work/no-extension" ]]
printf '#!/bin/sh\nexit 0\n' > "$work/not-elf"
chmod +x "$work/not-elf"
reject "$work/not-elf" "$version" "$work/not-elf.deb"
[[ ! -e "$work/not-elf.deb" ]]
chmod 644 "$work/not-elf"
reject "$work/not-elf" "$version" "$work/not-executable.deb"
[[ ! -e "$work/not-executable.deb" ]]
cp /bin/true "$work/wrong-arch"
printf '\267\000' | dd of="$work/wrong-arch" bs=1 seek=18 conv=notrunc status=none
reject "$work/wrong-arch" "$version" "$work/wrong-arch.deb"
[[ ! -e "$work/wrong-arch.deb" ]]
ln -s "$work/absent.deb" "$work/symlink.deb"
reject "$binary" "$version" "$work/symlink.deb"
[[ -L "$work/symlink.deb" && ! -e "$work/absent.deb" ]]
printf 'PASS: package contents, metadata, ownership, and rejected inputs\n'
