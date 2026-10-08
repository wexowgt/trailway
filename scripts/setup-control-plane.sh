#!/usr/bin/env bash
# Prepares a host to run the Trailway control plane (Postgres, API, web) with
# docker-compose.prod.yml. The web front is set up by setup-nginx-front.sh.
# Safe to re-run. Run as root after scripts/setup-host.sh.
#   CONTROL_DOMAIN=trailway.178-104-208-91.sslip.io ./setup-control-plane.sh
set -euo pipefail

CONTROL_DOMAIN="${CONTROL_DOMAIN:?set CONTROL_DOMAIN, e.g. trailway.178-104-208-91.sslip.io}"
APP_DIR="${APP_DIR:-/opt/trailway}"
HERE="$(cd "$(dirname "$0")" && pwd)"
DEPLOY_DIR="${DEPLOY_DIR:-$HERE/../deploy}"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }

if ! command -v docker >/dev/null 2>&1; then
  curl -fsSL https://get.docker.com | sh
fi
systemctl enable --now docker

mkdir -p "$APP_DIR"
if [ ! -f "$APP_DIR/.env" ]; then
  (umask 077
  cat > "$APP_DIR/.env" <<ENV
POSTGRES_PASSWORD=$(head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9')
IMAGE_PREFIX=ghcr.io/wexowgt
IMAGE_TAG=latest
ENV
  )
fi

# Backups: nightly at 03:15, last 7 kept.
install -m 0755 "$DEPLOY_DIR/backup-db.sh" /usr/local/bin/trailway-backup
cat > /etc/cron.d/trailway-backup <<CRON
15 3 * * * root COMPOSE_DIR=$APP_DIR /usr/local/bin/trailway-backup >> /var/log/trailway-backup.log 2>&1
CRON

echo "control plane host ready: $APP_DIR (.env). Next: scripts/setup-nginx-front.sh (nginx owns 80/443 here), then deploy."
