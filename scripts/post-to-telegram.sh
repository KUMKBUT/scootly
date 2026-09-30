#!/usr/bin/env bash
set -euo pipefail

# Публикация постера + caption в Telegram-канал.
# НЕ запускать без явного разрешения владельца канала.
#
# Требуемые переменные окружения:
#   TELEGRAM_BOT_TOKEN — токен бота (@BotFather)
#   TELEGRAM_CHANNEL   — id или @username канала
#
# caption.md уже экранирован под MarkdownV2 (парсер Telegram).

BOT_TOKEN="${TELEGRAM_BOT_TOKEN:?set TELEGRAM_BOT_TOKEN}"
CHANNEL="${TELEGRAM_CHANNEL:?set TELEGRAM_CHANNEL}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHOTO="${ROOT}/docs/architecture/exports/poster-telegram.png"
CAPTION_FILE="${ROOT}/scripts/poster/caption.md"

test -f "${PHOTO}" || { echo "poster not found: ${PHOTO}" >&2; exit 1; }
test -f "${CAPTION_FILE}" || { echo "caption not found: ${CAPTION_FILE}" >&2; exit 1; }

CAPTION=$(cat "${CAPTION_FILE}")

# Dry-run по умолчанию: покажет, что будет отправлено.
# Реальная отправка: POST_TO_TELEGRAM=1 ./scripts/post-to-telegram.sh
if [[ "${POST_TO_TELEGRAM:-0}" != "1" ]]; then
  echo "DRY RUN — ничего не отправлено."
  echo "  photo:   ${PHOTO} ($(du -h "${PHOTO}" | cut -f1))"
  echo "  channel: ${CHANNEL}"
  echo "  caption: $(wc -m < "${CAPTION_FILE}") chars"
  echo "  mode:    $([[ "${SEND_AS_DOCUMENT:-0}" == "1" ]] && echo document || echo photo)"
  echo "Отправить по-настоящему: POST_TO_TELEGRAM=1 $0"
  exit 0
fi

# SEND_AS_DOCUMENT=1 — отправить файлом без пережатия (Telegram
# ресайзит обычные фото, детализация при зуме теряется).
if [[ "${SEND_AS_DOCUMENT:-0}" == "1" ]]; then
  curl -s -X POST "https://api.telegram.org/bot${BOT_TOKEN}/sendDocument" \
    -F "chat_id=${CHANNEL}" \
    -F "document=@${PHOTO}" \
    -F "caption=${CAPTION}" \
    -F "parse_mode=MarkdownV2"
else
  curl -s -X POST "https://api.telegram.org/bot${BOT_TOKEN}/sendPhoto" \
    -F "chat_id=${CHANNEL}" \
    -F "photo=@${PHOTO}" \
    -F "caption=${CAPTION}" \
    -F "parse_mode=MarkdownV2"
fi
