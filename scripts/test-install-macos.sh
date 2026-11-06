#!/usr/bin/env bash
# Hermetic installer tests; no macOS or network access required.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
mkdir -p "$test_dir/bin"
cat > "$test_dir/bin/uname" <<'STUB'
#!/bin/bash
[[ $1 == -s ]] && echo Darwin || echo arm64
STUB
cat > "$test_dir/bin/curl" <<'STUB'
#!/bin/bash
while [[ $# -gt 0 ]]; do
  case $1 in --output) output=$2; shift 2 ;; *) shift ;; esac
done
case $output in
  *.sha256)
    name=${output%.sha256}
    if [[ ${CHECKSUM_MODE:-} == other ]]; then
      printf decoy > decoy.zip
      shasum -a 256 decoy.zip > "$output"
    elif [[ ${CHECKSUM_MODE:-} == extra ]]; then
      shasum -a 256 "$name" > "$output"
      shasum -a 256 "$name" >> "$output"
    elif [[ ${CHECKSUM_MODE:-} == mismatch ]]; then
      printf '%064d  %s\n' 0 "$name" > "$output"
    else
      shasum -a 256 "$name" > "$output"
    fi ;;
  *) printf 'downloaded zip' > "$output" ;;
esac
STUB
cat > "$test_dir/bin/ditto" <<'STUB'
#!/bin/bash
if [[ $1 != -x ]]; then
  if [[ ${COPY_MODE:-} == fail ]]; then
    mkdir -p "$2"; printf partial > "$2/partial"; exit 1
  fi
  cp -R "$1" "$2"
  exit
fi
mkdir -p "$4/agent-hub.app/Contents/MacOS"
printf new > "$4/agent-hub.app/Contents/MacOS/agent-hub"
chmod +x "$4/agent-hub.app/Contents/MacOS/agent-hub"
touch "$4/agent-hub.app/Contents/Info.plist"
STUB
cat > "$test_dir/bin/pgrep" <<'STUB'
#!/bin/bash
exit 1
STUB
cat > "$test_dir/bin/ruby" <<'STUB'
#!/bin/bash
# Inject installation failure or a concurrent destination at the final move.
[[ $# == 5 && $1 == -rfiddle && $2 == -e ]] || exit 2
source=$4
if [[ ${MOVE_MODE:-} == unsupported && $source == */check ]]; then exit 1; fi
if [[ ${MOVE_MODE:-} == unsafe && $source == */check ]]; then /bin/mv -f "$4" "$5"; exit; fi
case $source in
  *unpack/agent-hub.app|*/.agent-hub-install.*/agent-hub.app)
    if [[ ${MOVE_MODE:-} == fail ]]; then exit 1; fi
    if [[ ${MOVE_MODE:-} == collision ]]; then mkdir -p "$APPS_DIR/agent-hub.app"; printf concurrent > "$APPS_DIR/agent-hub.app/owner"; fi
    if [[ ${MOVE_MODE:-} == symlink_collision ]]; then ln -s missing "$APPS_DIR/agent-hub.app"; fi ;;
esac
if [[ $(/usr/bin/uname -s) == Darwin ]]; then
  exec /usr/bin/ruby "$@"
fi
# Linux equivalent of Darwin RENAME_EXCL; use an actual exclusive rename.
exec /usr/bin/perl -e 'require "syscall.ph"; my ($from, $to) = @ARGV; syscall(SYS_renameat2(), -100, $from, -100, $to, 1) == 0 or die "rename: $!\n"' "$4" "$5"
STUB
chmod +x "$test_dir/bin/"*
export PATH="$test_dir/bin:$PATH"
failures=0
check() { if ! "$@"; then printf 'FAIL: %s\n' "$case_name" >&2; cat "$test_dir/log" >&2; failures=$((failures + 1)); else printf 'PASS: %s\n' "$case_name"; fi; }
run() { APPS_DIR="$apps" CHECKSUM_MODE=${CHECKSUM_MODE:-} MOVE_MODE=${MOVE_MODE:-} COPY_MODE=${COPY_MODE:-} bash "$root/scripts/install-macos.sh" v0.10.0 > "$test_dir/log" 2>&1; }
setup() { apps="$test_dir/$1/apps with spaces"; mkdir -p "$apps"; unset CHECKSUM_MODE MOVE_MODE COPY_MODE; }
rejected() { ! run; }
setup backup
mkdir -p "$apps/agent-hub.app" "$apps/agent-hub.app.old"
printf original > "$apps/agent-hub.app/marker"
printf keep > "$apps/agent-hub.app.old/marker"
case_name='existing backup is preserved'; check rejected
case_name='existing app and backup are untouched'; check bash -c '[[ $(cat "$1/agent-hub.app/marker") == original && $(cat "$1/agent-hub.app.old/marker") == keep ]]' _ "$apps"
setup backup_link
ln -s missing "$apps/agent-hub.app.old"
case_name='dangling backup symlink is preserved'; check rejected
case_name='dangling backup symlink remains'; check bash -c '[[ -L "$1/agent-hub.app.old" && $(readlink "$1/agent-hub.app.old") == missing ]]' _ "$apps"
setup checksum
CHECKSUM_MODE=other
case_name='checksum naming another file is rejected'; check rejected
case_name='rejected checksum does not install'; check bash -c '[[ ! -e "$1/agent-hub.app" ]]' _ "$apps"
setup extra
CHECKSUM_MODE=extra
case_name='multiple checksum entries are rejected'; check rejected
setup mismatch
CHECKSUM_MODE=mismatch
case_name='incorrect checksum is rejected'; check rejected
setup copy_failure
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
COPY_MODE=fail
case_name='partial staged copy is rejected'; check rejected
case_name='partial staged copy leaves app intact'; check bash -c '[[ $(cat "$1/agent-hub.app/marker") == original && ! -e "$1/agent-hub.app.old" ]]' _ "$apps"
setup unsupported
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
MOVE_MODE=unsupported
case_name='unsupported exclusive rename is rejected'; check rejected
case_name='unsupported exclusive rename leaves app intact'; check bash -c '[[ $(cat "$1/agent-hub.app/marker") == original && ! -e "$1/agent-hub.app.old" ]]' _ "$apps"
setup unsafe
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
MOVE_MODE=unsafe
case_name='rename that overwrites is rejected'; check rejected
case_name='unsafe rename leaves app intact'; check bash -c '[[ $(cat "$1/agent-hub.app/marker") == original && ! -e "$1/agent-hub.app.old" ]]' _ "$apps"
setup rollback
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
MOVE_MODE=fail
case_name='failed install is rejected'; check rejected
case_name='failed install restores original'; check bash -c '[[ $(cat "$1/agent-hub.app/marker") == original ]]' _ "$apps"
setup first_failure
MOVE_MODE=fail
case_name='first install failure is rejected'; check rejected
case_name='first install failure does not claim an old version was restored'; check bash -c '! grep -q "старая версия возвращена" "$1"' _ "$test_dir/log"
setup symlink
ln -s missing "$apps/agent-hub.app"
MOVE_MODE=fail
case_name='failed install restores dangling symlink'; check bash -c '! APPS_DIR="$1" MOVE_MODE=fail bash "$2/scripts/install-macos.sh" v0.10.0 >/dev/null 2>&1 && [[ -L "$1/agent-hub.app" && $(readlink "$1/agent-hub.app") == missing ]]' _ "$apps" "$root"
setup collision
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
MOVE_MODE=collision
case_name='concurrent destination is rejected'; check rejected
case_name='concurrent destination and backup survive'; check bash -c '[[ $(cat "$1/agent-hub.app/owner") == concurrent && $(cat "$1/agent-hub.app.old/agent-hub.app/marker") == original ]]' _ "$apps"
setup symlink_collision
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
MOVE_MODE=symlink_collision
case_name='concurrent dangling symlink is rejected'; check rejected
case_name='concurrent dangling symlink and backup survive'; check bash -c '[[ -L "$1/agent-hub.app" && $(readlink "$1/agent-hub.app") == missing && $(cat "$1/agent-hub.app.old/agent-hub.app/marker") == original ]]' _ "$apps"
setup success
mkdir -p "$apps/agent-hub.app"; printf original > "$apps/agent-hub.app/marker"
case_name='normal upgrade succeeds'; check run
case_name='normal upgrade installs new app and cleans backup'; check bash -c '[[ $(cat "$1/agent-hub.app/Contents/MacOS/agent-hub") == new && ! -e "$1/agent-hub.app.old" ]]' _ "$apps"
[[ $failures == 0 ]] || { printf '%s failure(s)\n' "$failures" >&2; exit 1; }
