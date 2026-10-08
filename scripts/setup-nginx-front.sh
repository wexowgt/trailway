#!/usr/bin/env bash
# For a host where nginx already owns ports 80 and 443 (other sites live
# there). nginx stays in front: a stream block splits :443 by TLS server name,
# the control plane gets its own vhost, and Caddy (agent service routes) moves
# to 127.0.0.1:8443. Changes live nginx config: /etc/nginx is backed up first
# and restored if `nginx -t` fails. Run as root.
#   CONTROL_DOMAIN=trailway.178-104-208-91.sslip.io ./setup-nginx-front.sh
set -euo pipefail

CONTROL_DOMAIN="${CONTROL_DOMAIN:?set CONTROL_DOMAIN, e.g. trailway.178-104-208-91.sslip.io}"
SERVICE_SUFFIX="${SERVICE_SUFFIX:-${CONTROL_DOMAIN#*.}}"
SERVICE_SUFFIX_RE="${SERVICE_SUFFIX//./\\\\.}" # doubled backslash survives sed
HERE="$(cd "$(dirname "$0")" && pwd)"
DEPLOY_DIR="${DEPLOY_DIR:-$HERE/../deploy}"
BACKUP="/root/nginx-backup-$(date +%Y%m%d-%H%M%S).tar.gz"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }

tar -czf "$BACKUP" -C / etc/nginx
echo "nginx backup: $BACKUP"
restore() {
  echo "nginx -t failed, restoring $BACKUP" >&2
  rm -rf /etc/nginx && tar -xzf "$BACKUP" -C /
  exit 1
}

command -v certbot >/dev/null 2>&1 || apt-get install -y -qq certbot
nginx -V 2>&1 | grep -q -- --with-stream || apt-get install -y -qq libnginx-mod-stream

render() {
  sed -e "s/__CONTROL_DOMAIN__/$CONTROL_DOMAIN/g" \
      -e "s/__SERVICE_SUFFIX_RE__/$SERVICE_SUFFIX_RE/g" \
      -e "s/__SERVICE_SUFFIX__/$SERVICE_SUFFIX/g" "$1"
}

# 1. Port 80 vhost first, so the certificate can be issued over HTTP-01.
mkdir -p /var/www/letsencrypt
if [ ! -d "/etc/letsencrypt/live/$CONTROL_DOMAIN" ]; then
  cat > /etc/nginx/sites-available/trailway <<CONF
server {
    listen 80;
    listen [::]:80;
    server_name $CONTROL_DOMAIN;
    location /.well-known/acme-challenge/ { root /var/www/letsencrypt; }
    location / { return 301 https://\$host\$request_uri; }
}
CONF
  ln -sf /etc/nginx/sites-available/trailway /etc/nginx/sites-enabled/trailway
  nginx -t || restore
  systemctl reload nginx
  certbot certonly --webroot -w /var/www/letsencrypt -d "$CONTROL_DOMAIN" \
    --non-interactive --agree-tos --register-unsafely-without-email
fi

# 2. Move every existing :443 listener behind the stream split.
for f in /etc/nginx/sites-enabled/* /etc/nginx/conf.d/*.conf; do
  [ -f "$f" ] || continue
  [ "$f" = /etc/nginx/sites-enabled/trailway ] && continue
  sed -i -E \
    -e 's/^([[:space:]]*)listen[[:space:]]+\[::\]:443([^;]*);/\1# moved behind stream split: listen [::]:443\2;/' \
    -e 's/^([[:space:]]*)listen[[:space:]]+443([^;]*);/\1listen 127.0.0.1:4443\2;/' "$f"
done

# 3. Control plane vhost and the stream block.
render "$DEPLOY_DIR/nginx/trailway.conf.tmpl" > /etc/nginx/sites-available/trailway
ln -sf /etc/nginx/sites-available/trailway /etc/nginx/sites-enabled/trailway
mkdir -p /etc/nginx/stream.d
render "$DEPLOY_DIR/nginx/stream.conf.tmpl" > /etc/nginx/stream.d/trailway.conf
grep -q 'stream.d/\*.conf' /etc/nginx/nginx.conf \
  || printf '\nstream {\n    include /etc/nginx/stream.d/*.conf;\n}\n' >> /etc/nginx/nginx.conf
nginx -t || restore
systemctl restart nginx

# 4. Caddy only needs its admin API; the agent listens on 127.0.0.1:8443.
install -m 0644 "$DEPLOY_DIR/Caddyfile" /etc/caddy/Caddyfile
systemctl restart caddy
mkdir -p /etc/systemd/system/trailway-agent.service.d
printf '[Service]\nEnvironment=TRAILWAY_CADDY_LISTEN=127.0.0.1:8443\n' \
  > /etc/systemd/system/trailway-agent.service.d/caddy.conf
systemctl daemon-reload
systemctl restart trailway-agent 2>/dev/null || true
echo "done. Restore the old nginx config with: rm -rf /etc/nginx && tar -xzf $BACKUP -C / && systemctl restart nginx"
