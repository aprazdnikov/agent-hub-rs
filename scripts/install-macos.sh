#!/usr/bin/env bash
# Установка agent-hub на macOS без карантина Gatekeeper: curl не навешивает пометку,
# в отличие от браузера. Проверка sha256 обязательна, бандл меняется атомарно.
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
shasum -a 256 -c "${name}.app.zip.sha256" >/dev/null || fail 'контрольная сумма не совпала — загрузку не устанавливаем'

ditto -x -k "${name}.app.zip" unpack
[[ -x unpack/agent-hub.app/Contents/MacOS/agent-hub ]] || fail 'в архиве нет исполняемого файла бандла'
[[ -f unpack/agent-hub.app/Contents/Info.plist ]] || fail 'в архиве нет Info.plist'

if pgrep -f 'agent-hub\.app/Contents/MacOS/agent-hub' >/dev/null 2>&1; then
  fail 'agent-hub запущен — выйдите из него (значок в трее → Выход) и повторите'
fi

mkdir -p "$apps_dir"
app="$apps_dir/agent-hub.app"
# swap через резервную копию: при неудачном перемещении старая версия возвращается
if [[ -e $app || -L $app ]]; then mv "$app" "${app}.old"; fi
if ! mv "unpack/agent-hub.app" "$app"; then
  [[ -e "${app}.old" && ! -e $app ]] && mv "${app}.old" "$app"
  fail 'не удалось установить бандл — старая версия возвращена'
fi
if [[ -e "${app}.old" ]]; then rm -rf "${app}.old"; fi

note 'готово'
printf 'Установлено: %s (%s) в %s\n' "$tag" "$target" "$app"
printf 'Запуск:      open "%s"\n' "$app"
