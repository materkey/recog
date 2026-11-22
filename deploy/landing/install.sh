#!/bin/sh
# Install the public Linux release for the current desktop user.
set -eu

fail() { printf 'Recog: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }
fetch() { curl -qfsSL --proto '=https' --proto-redir '=https' --retry 3 -o "$2" "$1"; }

has_sound_server() {
    has pulseaudio || has pipewire-pulse || pactl info >/dev/null 2>&1
}

has_vulkan_driver() {
    if [ -n "${VK_DRIVER_FILES:-}${VK_ICD_FILENAMES:-}" ]; then return 0; fi
    for driver in /usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json /usr/local/share/vulkan/icd.d/*.json; do
        if [ -r "$driver" ]; then return 0; fi
    done
    return 1
}

install_dependencies() {
    if has ffmpeg && has pactl && has parec && has_sound_server && has_vulkan_driver; then return; fi
    # ID_LIKE includes parent distributions, for example Debian on Ubuntu.
    # shellcheck source=/dev/null
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *' debian '*|*' ubuntu '*) manager=apt-get; audio=pulseaudio-utils; video=ffmpeg; vulkan=mesa-vulkan-drivers ;;
        *' fedora '*) manager=dnf; audio=pulseaudio-utils; video=ffmpeg-free; vulkan=mesa-vulkan-drivers ;;
        *' arch '*|*' manjaro '*) manager=pacman; audio=libpulse; video=ffmpeg; vulkan='vulkan-swrast' ;;
        *) fail "Дистрибутив не распознан. Для ручной установки нужны ffmpeg, pactl, parec, PulseAudio или PipeWire с поддержкой PulseAudio и драйвер Vulkan." ;;
    esac
    set --
    if ! has ffmpeg; then set -- "$@" "$video"; fi
    if ! has pactl || ! has parec; then set -- "$@" "$audio"; fi
    new_sound_server=false
    if ! has_sound_server; then
        new_sound_server=true
        if [ "$manager" = dnf ]; then set -- "$@" pipewire-pulseaudio; else set -- "$@" pipewire-pulse; fi
        set -- "$@" wireplumber
    fi
    if ! has_vulkan_driver; then set -- "$@" "$vulkan"; fi
    has sudo || fail "Для установки системных пакетов ($*) нужен sudo."
    printf 'Устанавливаю системные пакеты: %s\n' "$*"
    case "$manager" in
        apt-get) sudo apt-get update && sudo apt-get install -y --no-install-recommends "$@" ;;
        dnf) sudo dnf install -y "$@" ;;
        pacman) sudo pacman -S --needed --noconfirm "$@" ;;
    esac
    if [ "$new_sound_server" = true ]; then
        systemctl --user daemon-reload
        systemctl --user start wireplumber.service pipewire-pulse.socket
    fi
    if ! has ffmpeg || ! has pactl || ! has parec; then
        fail "После установки пакетов не найдены ffmpeg, pactl или parec."
    fi
}

main() {
    [ "$(uname -s)/$(uname -m)" = Linux/x86_64 ] || fail "Готовая сборка предназначена для Linux x86-64."
    [ "$(id -u)" != 0 ] || fail "Запустите команду от обычного пользователя, без sudo."
    systemctl --user show-environment >/dev/null 2>&1 || fail "Откройте терминал в сеансе пользователя с systemd и повторите команду."

    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' 0
    trap 'exit 1' HUP INT TERM
    (
        cd "$tmp"
        releases=https://github.com/materkey/recog/releases
        fetch "$releases/latest/download/SHA256SUMS" sums
        awk '{sub(/^\.\//, "", $2)}
             $2 ~ /^recog-v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?-linux-x86_64\.tar\.gz$/ &&
             length($1) == 64 && $1 ~ /^[[:xdigit:]]+$/ {print $1 "  " $2; found++}
             END {if (found != 1) exit 1}' sums > expected || fail "В последнем релизе не найдена однозначная контрольная сумма сборки Linux."
        read -r _ archive < expected
        tag=${archive#recog-}
        tag=${tag%-linux-x86_64.tar.gz}
        printf 'Скачиваю Recog %s…\n' "$tag"
        fetch "$releases/download/$tag/$archive" "$archive"
        sha256sum -c expected || fail "Контрольная сумма скачанного архива не совпала."
        tar -xzf "$archive"
        [ "$(cat recog/recog-release)" = "$tag" ] || fail "Версия в архиве не совпала с версией релиза."
        recog/bin/recog --help >/dev/null || fail "Сборка не запустилась. Проверьте версию glibc в сообщении выше."
        install_dependencies
        PATH="$PATH:$HOME/.local/bin" recog/bin/recog setup
    )
}

main "$@"
