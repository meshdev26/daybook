#!/usr/bin/env bash
# restore.sh -- restore the SQLite database from a backup file.
#
# Usage: scripts/restore.sh <backup.db>   (or latest in backups/)
# Stops nothing itself; operator should stop the service first.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "ERROR: cannot locate repo root" >&2; exit 1; }
ROOT="$(pwd)"

SRC="${1:-}"
if [[ -z "$SRC" ]]; then
    # pick newest backup
    SRC="$(ls -t "$ROOT"/backups/agent-*.db 2>/dev/null | head -n1)"
fi
[[ -f "$SRC" ]] || { echo "ERROR: backup file not found: $SRC" >&2; exit 1; }

ENV_PATH=""
for f in "$ROOT/.env" "/opt/telegram-agent/.env"; do
    [[ -f "$f" ]] && { ENV_PATH="$f"; break; }
done
if [[ -n "$ENV_PATH" ]]; then
    while IFS='=' read -r k v || [[ -n "$k" ]]; do
        case "$k" in
            [A-Z_]*) export "$k=$v" ;;
        esac
    done < "$ENV_PATH"
fi

DATA="${DATABASE_DIR:-}"
[[ -z "$DATA" && -n "${DATABASE_URL:-}" ]] && DATA="${DATABASE_URL#sqlite:}"
[[ -z "$DATA" ]] && DATA="$ROOT/data"
DB="$DATA/agent.db"

mkdir -p "$DATA" || { echo "ERROR: cannot create $DATA" >&2; exit 1; }

# Safety: keep a pre-restore snapshot.
if [[ -f "$DB" ]]; then
    cp -- "$DB" "$DB.pre-restore-$(date +%Y%m%d-%H%M%S)" || echo "warn: could not snapshot current db" >&2
fi

if command -v sqlite3 >/dev/null; then
    # Restore via sqlite3 .restore for WAL consistency.
    sqlite3 "$DB" ".restore '$SRC'" || { echo "ERROR: sqlite3 .restore failed" >&2; exit 1; }
else
    cp -- "$SRC" "$DB" || { echo "ERROR: copy failed" >&2; exit 1; }
fi

chmod 600 "$DB" 2>/dev/null || true
echo "Restored $DB from $SRC"
echo "Restart the service to use the restored database."