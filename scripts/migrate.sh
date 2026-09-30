#!/usr/bin/env bash
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is not set (see .env.example)}"

echo "Applying sqlx migrations from backend/crates/db/migrations..."
cargo run -p db --help >/dev/null 2>&1 || true
sqlx migrate run --source backend/crates/db/migrations --database-url "$DATABASE_URL"
