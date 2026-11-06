#!/usr/bin/env bash
# Compile an NSIS installer from a supplied executable without installing it.
set -euo pipefail
fail() { printf 'test-windows-package: %s\n' "$*" >&2; exit 1; }
if [[ ${1:-} == --self-test && $# == 1 ]]; then
  work=$(mktemp -d "${TMPDIR:-/tmp}/agent-hub-nsis-test.XXXXXX")
  trap 'rm -rf "$work"' EXIT
  mkdir "$work/with spaces"
  pe="$work/with spaces/x64.exe"
  # A synthetic PE32+ header with one section for testing the input gate.
  dd if=/dev/zero of="$pe" bs=240 count=1 2>/dev/null
  printf 'MZ' | dd of="$pe" bs=1 seek=0 conv=notrunc 2>/dev/null
  printf '\100\000\000\000PE\000\000\144\206\001\000' | dd of="$pe" bs=1 seek=60 conv=notrunc 2>/dev/null
  printf '\160\000\002\000\013\002' | dd of="$pe" bs=1 seek=84 conv=notrunc 2>/dev/null
  cp "$pe" "$work/x86.exe"
  printf '\114\001' | dd of="$work/x86.exe" bs=1 seek=68 conv=notrunc 2>/dev/null
  cp "$pe" "$work/arm64.exe"
  printf '\144\252' | dd of="$work/arm64.exe" bs=1 seek=68 conv=notrunc 2>/dev/null
  cp "$pe" "$work/pe32.exe"
  printf '\013\001' | dd of="$work/pe32.exe" bs=1 seek=88 conv=notrunc 2>/dev/null
  printf '\177ELF' > "$work/elf"
  printf 'MZ' > "$work/truncated.exe"
  for invalid in "$work/x86.exe" "$work/arm64.exe" "$work/pe32.exe" "$work/elf" "$work/truncated.exe"; do
    if bash "$0" "$invalid" 1.2.3 "$work/invalid-output.exe"; then fail "accepted $invalid"; fi
    [[ ! -e $work/invalid-output.exe ]] || fail 'created output for invalid input'
  done
  bash "$0" "$pe" 1.2.3 "$work/with spaces/installer.exe"
  cp "$work/with spaces/installer.exe" "$work/original.exe"
  if bash "$0" "$pe" 1.2.3 "$work/with spaces/installer.exe"; then fail 'accepted existing output'; fi
  cmp "$work/original.exe" "$work/with spaces/installer.exe" || fail 'existing output changed'
  printf 'Windows packaging input checks passed\n'
  exit 0
fi
[[ $# == 3 ]] || fail 'usage: bash scripts/test-windows-package.sh BINARY VERSION OUTPUT.exe'
command -v makensis >/dev/null || fail 'makensis is required'
[[ -f $1 ]] || fail 'BINARY must exist'
[[ $2 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'VERSION must be X.Y.Z'
[[ $3 == *.exe ]] || fail 'OUTPUT must end in .exe'
[[ ! -e $3 && ! -L $3 ]] || fail 'OUTPUT already exists'
# Check the PE signature, x86-64 machine and PE32+ optional header without
# executing the supplied file. od is available in Git Bash and on Unix hosts.
read -r mz1 mz2 < <(od -An -v -tu1 -N2 "$1") || :
[[ ${mz1:-} == 77 && ${mz2:-} == 90 ]] || fail 'BINARY must be a Windows x86-64 PE32+ executable'
read -r o1 o2 o3 o4 < <(od -An -v -tu1 -j60 -N4 "$1" 2>/dev/null || :) || :
[[ -n ${o4:-} ]] || fail 'BINARY has a truncated PE header'
offset=$((o1 + (o2 << 8) + (o3 << 16) + (o4 << 24)))
read -r p1 p2 p3 p4 machine1 machine2 < <(od -An -v -tu1 -j "$offset" -N6 "$1" 2>/dev/null || :) || :
[[ ${p1:-} == 80 && ${p2:-} == 69 && ${p3:-} == 0 && ${p4:-} == 0 && ${machine1:-} == 100 && ${machine2:-} == 134 ]] || fail 'BINARY must be a Windows x86-64 PE32+ executable'
read -r magic1 magic2 < <(od -An -v -tu1 -j "$((offset + 24))" -N2 "$1" 2>/dev/null || :) || :
[[ ${magic1:-} == 11 && ${magic2:-} == 2 ]] || fail 'BINARY must be a Windows x86-64 PE32+ executable'
read -r s1 s2 < <(od -An -v -tu1 -j "$((offset + 6))" -N2 "$1" 2>/dev/null || :) || :
read -r h1 h2 c1 c2 < <(od -An -v -tu1 -j "$((offset + 20))" -N4 "$1" 2>/dev/null || :) || :
[[ -n ${s2:-} && -n ${c2:-} ]] || fail 'BINARY has a truncated PE header'
sections=$((s1 + (s2 << 8)))
header_size=$((h1 + (h2 << 8)))
characteristics=$((c1 + (c2 << 8)))
file_size=$(wc -c < "$1")
(( sections > 0 && header_size >= 112 && (characteristics & 2) != 0 && (characteristics & 0x2000) == 0 && file_size >= offset + 24 + header_size + sections * 40 )) || fail 'BINARY has an invalid or truncated executable header'
root=$(cd "$(dirname "$0")/.." && pwd)
binary=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
output_dir=$(cd "$(dirname "$3")" && pwd)
output="$output_dir/$(basename "$3")"
license="$root/LICENSE"
source="$root/packaging/windows/agent-hub.nsi"
[[ -f $source ]] || fail 'installer source is missing'
# Git Bash paths embedded in -D arguments are not converted automatically.
if command -v cygpath >/dev/null; then
  binary=$(cygpath -w "$binary")
  output=$(cygpath -w "$output")
  license=$(cygpath -w "$license")
  source=$(cygpath -w "$source")
fi
makensis -V2 "-DINPUT_EXE=$binary" "-DLICENSE_FILE=$license" "-DOUTPUT_EXE=$output" "-DVERSION=$2" "$source"
[[ -s $3 ]] || fail 'compiler did not produce an installer'
printf 'Created %s\n' "$3"
