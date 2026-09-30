#!/usr/bin/env bash
set -euo pipefail

# Суточный бэкап PostgreSQL в S3 (MinIO). Запуск: cron `0 3 * * * ./scripts/backup.sh`.
: "${DATABASE_URL:?DATABASE_URL is not set (see .env.example)}"
: "${S3_BACKUP_BUCKET:=scootly-backups}"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
tmp="/tmp/scootly-${stamp}.sql.gz"

pg_dump "$DATABASE_URL" | gzip > "$tmp"
echo "dump: $tmp ($(du -h "$tmp" | cut -f1))"

if command -v mc >/dev/null 2>&1 && [ -n "${S3_ALIAS:-}" ]; then
  mc cp "$tmp" "${S3_ALIAS}/${S3_BACKUP_BUCKET}/postgres/${stamp}.sql.gz"
  echo "uploaded to s3://${S3_BACKUP_BUCKET}/postgres/${stamp}.sql.gz"
else
  echo "mc not configured (S3_ALIAS) — dump left at $tmp"
fi
