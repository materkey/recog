#!/usr/bin/env bash
#
# backup-data.sh — снимок пользовательских данных recog (вне git).
#
# Что сохраняется: meetings/ (транскрипты + аудио), speakers*.json (база
# эмбеддингов), user-profile.json, ai-settings.json, конфиг установки
# config.toml (~/.config/recog, docs/configuration.md), .env / recog/web/.env (секреты).
# logs/ — только с --with-logs.
#
# Usage:
#   ./scripts/backup-data.sh                        # снимок в ~/backups/recog-<дата>
#   ./scripts/backup-data.sh --dest /Volumes/T7/recog
#   ./scripts/backup-data.sh --remote <backup-host>:backups/recog
#   ./scripts/backup-data.sh --with-logs --remote <backup-host>:backups/recog
#
# Снимок делается rsync'ом, поэтому повторный запуск в тот же --dest
# докидывает только изменившееся. Ничего не удаляет в репозитории.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"
DATA_DIR="$(bash "$SCRIPT_DIR/scripts/data-dir.sh")"
CONFIG_FILE="$(bash "$SCRIPT_DIR/scripts/config-path.sh")"

DEST="$HOME/backups/recog-$(date +%Y%m%d-%H%M)"
REMOTE=""
WITH_LOGS=0

while [ $# -gt 0 ]; do
    case "$1" in
        --dest)       DEST="$2"; shift 2 ;;
        --remote)     REMOTE="$2"; shift 2 ;;
        --with-logs)  WITH_LOGS=1; shift ;;
        -h|--help)    sed -n '2,20p' "$0"; exit 0 ;;
        *)            echo "Неизвестный аргумент: $1" >&2; exit 2 ;;
    esac
done

mkdir -p "$DEST"

echo "=== Локальный снимок → $DEST"
[ -d "$DATA_DIR/meetings" ] && rsync -a "$DATA_DIR/meetings/" "$DEST/meetings/"
if [ "$WITH_LOGS" = "1" ] && [ -d "$DATA_DIR/logs" ]; then
    rsync -a "$DATA_DIR/logs/" "$DEST/logs/"
fi

for f in "$DATA_DIR"/speakers*.json; do
    [ -f "$f" ] && cp -p "$f" "$DEST/"
done
if [ -f "$DATA_DIR/user-profile.json" ]; then
    (umask 077; cp "$DATA_DIR/user-profile.json" "$DEST/user-profile.json")
    chmod 600 "$DEST/user-profile.json"
fi
if [ -f "$DATA_DIR/ai-settings.json" ]; then
    (umask 077; cp "$DATA_DIR/ai-settings.json" "$DEST/ai-settings.json")
    chmod 600 "$DEST/ai-settings.json"
fi
if [ -f "$DATA_DIR/privacy.json" ]; then
    (umask 077; cp "$DATA_DIR/privacy.json" "$DEST/privacy.json")
    chmod 600 "$DEST/privacy.json"
fi
if [ -f "$CONFIG_FILE" ]; then
    (umask 077; cp "$CONFIG_FILE" "$DEST/config.toml")
    chmod 600 "$DEST/config.toml"
fi
# .env копируется под другим именем: иначе случайный `cp -r` обратно в репо
# перезапишет рабочий .env, а его правит только человек.
[ -f .env ] && { cp -p .env "$DEST/env.backup"; chmod 600 "$DEST/env.backup"; }
[ -f recog/web/.env ] && { cp -p recog/web/.env "$DEST/frontend-env.backup"; chmod 600 "$DEST/frontend-env.backup"; }

# Проверка целостности: сравниваем суммарный размер в байтах, а не du —
# du показывает занятые блоки и на разных ФС расходится при равном содержимом.
if [ -d "$DATA_DIR/meetings" ]; then
    total_bytes() { find "$1" -type f -exec stat -f%z {} + 2>/dev/null || find "$1" -type f -printf '%s\n'; }
    src=$(total_bytes "$DATA_DIR/meetings" | awk '{s+=$1} END{print s+0}')
    dst=$(total_bytes "$DEST/meetings" | awk '{s+=$1} END{print s+0}')
    echo "meetings: src=$src dst=$dst байт"
    [ "$src" = "$dst" ] || echo "WARNING: размеры расходятся (идёт запись митинга?) — перезапусти скрипт" >&2
fi

if [ -n "$REMOTE" ]; then
    echo "=== Копия на $REMOTE"
    # R2 сюда не годится: во встречах есть рабочий контент (см. CLAUDE.md).
    rsync -a --partial "$DEST/" "$REMOTE/"
    echo "Готово: $REMOTE"
fi

echo "=== Снимок готов: $DEST"
du -sh "$DEST"
