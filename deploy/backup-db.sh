#!/usr/bin/env bash
# Nightly Postgres dump, keeps the last 7. Run from cron (see setup-control-plane.sh).
set -euo pipefail
DIR="${BACKUP_DIR:-/var/backups/trailway}"
COMPOSE_DIR="${COMPOSE_DIR:-/opt/trailway}"
KEEP="${KEEP:-7}"
mkdir -p "$DIR"
out="$DIR/trailway-$(date +%Y%m%d-%H%M%S).sql.gz"
cd "$COMPOSE_DIR"
docker compose -f docker-compose.prod.yml exec -T postgres pg_dump -U trailway trailway | gzip > "$out.part"
mv "$out.part" "$out"
ls -1t "$DIR"/trailway-*.sql.gz | tail -n +$((KEEP + 1)) | xargs -r rm -f
ls -lh "$DIR"
