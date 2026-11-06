#!/usr/bin/env bash
# Установка agent-hub на macOS без карантина Gatekeeper: curl не навешивает пометку,
# в отличие от браузера. Проверка sha256 обязательна, при ошибке сохраняется резервная копия.
#
# Использование:
#   curl -fsSL https://raw.githubusercontent.com/aprazdnikov/agent-hub-rs/main/scripts/install-macos.sh | bash
#   bash scripts/install-macos.sh [ВЕРСИЯ]                  # или TAG=v0.9.0
#   APPS_DIR=/Applications sudo -E bash scripts/install-macos.sh   # системная папка
set -euo pipefail

fail() { printf 'install-macos: %s\n' "$*" >&2; exit 1; }
note() { printf 'install-macos: %s\n' "$*"; }

[[ $(uname -s) == Darwin ]] || fail 'только для macOS (используйте архивы релизов на других системах)'
command -v curl >/dev/null || fail 'нужен curl'
command -v shasum >/dev/null || fail 'нужен shasum (входит в macOS)'
command -v ditto >/dev/null || fail 'нужен ditto (входит в macOS)'
command -v ruby >/dev/null || fail 'нужен ruby (входит в macOS)'

repo='aprazdnikov/agent-hub-rs'
apps_dir=${APPS_DIR:-"$HOME/Applications"}
tag=${1:-${TAG:-}}

case $(uname -m) in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *) fail 'неизвестная архитектура: '"$(uname -m)" ;;
esac

if [[ -z $tag ]]; then
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${repo}/releases/latest")
  tag=${tag##*/}
fi
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "не понятен тег релиза: ${tag}"

name="agent-hub-${target}"
base="https://github.com/${repo}/releases/download/${tag}"
note "${tag} · ${target}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
curl -fsSL --output "${name}.app.zip" "${base}/${name}.app.zip" || fail 'не удалось скачать архив'
curl -fsSL --output "${name}.app.zip.sha256" "${base}/${name}.app.zip.sha256" || fail 'не удалось скачать контрольную сумму'
awk -v name="${name}.app.zip" '
  NR == 1 {
    hash = substr($0, 1, 64)
    rest = substr($0, 65)
    valid = length(hash) == 64 && hash ~ /^[0-9a-fA-F]+$/ && (rest == "  " name || rest == " *" name)
  }
  END { exit !(NR == 1 && valid) }
' "${name}.app.zip.sha256" || fail 'неверный файл контрольной суммы'
shasum -a 256 -c "${name}.app.zip.sha256" >/dev/null || fail 'контрольная сумма не совпала — загрузку не устанавливаем'

ditto -x -k "${name}.app.zip" unpack
[[ -x unpack/agent-hub.app/Contents/MacOS/agent-hub ]] || fail 'в архиве нет исполняемого файла бандла'
[[ -f unpack/agent-hub.app/Contents/Info.plist ]] || fail 'в архиве нет Info.plist'

if pgrep -f 'agent-hub\.app/Contents/MacOS/agent-hub' >/dev/null 2>&1; then
  fail 'agent-hub запущен — выйдите из него (значок в трее → Выход) и повторите'
fi

mkdir -p "$apps_dir"
app="$apps_dir/agent-hub.app"
backup="${app}.old"
stage=$(mktemp -d "$apps_dir/.agent-hub-install.XXXXXXXX") || fail 'не удалось подготовить папку установки'
cleanup() {
  rm -rf "$stage"
  [[ -z ${backup_owned:-} ]] || rmdir "$backup" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
ditto "unpack/agent-hub.app" "$stage/agent-hub.app" || fail 'не удалось подготовить бандл'

# macOS mv -n checks with access() before rename(), so it can replace a dangling
# symlink or a destination created concurrently. Ruby's Fiddle calls the system
# renamex_np with RENAME_EXCL (4); unsupported filesystems fail before backup.
rename_exclusive() {
  ruby -rfiddle -e '
    rename = Fiddle::Function.new(Fiddle::Handle::DEFAULT["renamex_np"],
      [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_INT], Fiddle::TYPE_INT)
    exit(rename.call(ARGV[0], ARGV[1], 4) == 0 ? 0 : 1)
  ' "$1" "$2"
}
: > "$stage/check"
: > "$stage/checked"
if rename_exclusive "$stage/check" "$stage/checked"; then
  fail 'файловая система не поддерживает безопасное перемещение'
fi
[[ -f "$stage/check" && -f "$stage/checked" ]] || fail 'не удалось проверить безопасное перемещение'
rm "$stage/checked"
rename_exclusive "$stage/check" "$stage/checked" || fail 'файловая система не поддерживает безопасное перемещение'
rm "$stage/checked"

# Резервная папка служит блокировкой и никогда не заменяет существующий путь.
mkdir -m 700 "$backup" 2>/dev/null || fail "резервная копия уже существует или недоступна: ${backup}"
backup_owned=1
if [[ -e $app || -L $app ]]; then
  rename_exclusive "$app" "$backup/agent-hub.app" || fail 'не удалось сохранить старую версию'
  [[ -e "$backup/agent-hub.app" || -L "$backup/agent-hub.app" ]] || fail 'не удалось сохранить старую версию'
fi

# Перемещение внутри одной файловой системы не заменяет существующий путь.
if ! rename_exclusive "$stage/agent-hub.app" "$app"; then
  if [[ -e "$backup/agent-hub.app" || -L "$backup/agent-hub.app" ]]; then
    rename_exclusive "$backup/agent-hub.app" "$app" || true
    if [[ -e "$backup/agent-hub.app" || -L "$backup/agent-hub.app" ]]; then
      fail "не удалось установить бандл — старая версия сохранена в ${backup}/agent-hub.app"
    fi
    fail 'не удалось установить бандл — старая версия возвращена'
  fi
  fail 'не удалось установить бандл'
fi
if [[ -e "$backup/agent-hub.app" || -L "$backup/agent-hub.app" ]]; then rm -rf "$backup/agent-hub.app"; fi

note 'готово'
printf 'Установлено: %s (%s) в %s\n' "$tag" "$target" "$app"
printf 'Запуск:      open "%s"\n' "$app"
