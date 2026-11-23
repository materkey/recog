#!/bin/sh
# Install Recog from its package repository and set it up for the current user:
#   curl -fsSL https://recog.vkkovalev.work/install.sh | sh
# apt or pacman then update it with the system (docs/linux-packages.md).
# Running it again is safe. --no-setup only installs the package.
set -eu

fingerprint=6E66571BB28E6EC3827CD813E2E87FE565091BC0
# Overrides for testing a copy of the repository.
repository=${RECOG_INSTALL_REPOSITORY:-https://github.com/materkey/recog/releases/latest/download}
key=${RECOG_INSTALL_KEY:-https://recog.vkkovalev.work/recog.asc}

fail() { printf 'Recog: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }
fetch() { curl -qfsSL --proto '=https' --proto-redir '=https' --retry 3 -o "$2" "$1"; }

glibc_at_least_2_39() {
    version=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed 's/^glibc //')
    major=${version%%.*}
    minor=${version#*.}
    minor=${minor%%.*}
    case "$major$minor" in '' | *[!0-9]*) return 1 ;; esac
    [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 39 ]; }
}

install_apt() {
    glibc_at_least_2_39 || fail "Нужна Ubuntu 24.04, Debian 13 или новее."
    printf 'Подключаю репозиторий Recog к apt и устанавливаю пакет recog.\n'
    sudo install -d -m 0755 /etc/apt/keyrings
    sudo install -m 0644 "$tmp/recog.asc" /etc/apt/keyrings/recog.asc
    printf 'Types: deb\nURIs: %s/\nSuites: ./\nSigned-By: /etc/apt/keyrings/recog.asc\n' "$repository" |
        sudo tee /etc/apt/sources.list.d/recog.sources >/dev/null
    sudo apt-get update
    sudo apt-get install -y recog
}

install_pacman() {
    printf 'Подключаю репозиторий Recog к pacman. Arch обновляет систему только целиком,\n'
    printf 'поэтому pacman -Syu сначала покажет все обновления и спросит подтверждение.\n'
    sudo pacman-key --add "$tmp/recog.asc"
    sudo pacman-key --lsign-key "$fingerprint"
    if ! grep -q '^\[recog\]' /etc/pacman.conf; then
        printf '\n[recog]\nSigLevel = PackageOptional DatabaseRequired\nServer = %s\n' "$repository" |
            sudo tee -a /etc/pacman.conf >/dev/null
    fi
    # The script itself arrives on stdin; pacman's question needs the terminal.
    if (true </dev/tty) 2>/dev/null; then
        # shellcheck disable=SC2024 # pacman reads the user's own terminal
        sudo pacman -Syu --needed recog </dev/tty
    else
        sudo pacman -Syu --needed --noconfirm recog
    fi
}

main() {
    setup=true
    for arg in "$@"; do
        case "$arg" in
            --no-setup) setup=false ;;
            *) fail "Неизвестный параметр $arg. Допустим только --no-setup." ;;
        esac
    done
    [ "$(uname -s)/$(uname -m)" = Linux/x86_64 ] || fail "Пакеты Recog собраны для Linux x86-64."
    [ "$(id -u)" != 0 ] || fail "Запустите команду от обычного пользователя, без sudo."
    has sudo || fail "Для установки пакета нужен sudo."
    if [ "$setup" = true ]; then
        systemctl --user show-environment >/dev/null 2>&1 ||
            fail "Откройте терминал в сеансе пользователя с systemd и повторите команду."
    fi
    # ID_LIKE includes parent distributions, for example debian on Ubuntu.
    # shellcheck source=/dev/null
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *' debian '* | *' ubuntu '*) manager=apt ;;
        *' arch '*) manager=pacman ;;
        *) fail "Репозиторий есть для Ubuntu, Debian, Arch и Manjaro. Для других систем скачайте архив linux-x86_64: https://github.com/materkey/recog#linux-x86-64" ;;
    esac

    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' 0
    trap 'exit 1' HUP INT TERM
    case "$key" in
        /*) cp "$key" "$tmp/recog.asc" ;;
        *) fetch "$key" "$tmp/recog.asc" || fail "Не удалось скачать ключ репозитория $key." ;;
    esac
    if has gpg; then
        mkdir -m 0700 "$tmp/gnupg"
        gpg --homedir "$tmp/gnupg" --show-keys --with-colons "$tmp/recog.asc" 2>/dev/null |
            grep -q "^fpr:.*:$fingerprint:" || fail "Скачанный ключ не совпал с ключом Recog $fingerprint."
    fi
    "install_$manager"

    if [ "$setup" = true ]; then
        recog-setup
    else
        printf 'Recog установлен. Настройте его для своего пользователя: recog-setup\n'
    fi
}

main "$@"
