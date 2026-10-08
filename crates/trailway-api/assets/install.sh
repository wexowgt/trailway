#!/bin/sh
# Trailway agent installer.
#   curl -fsSL <api>/install.sh | sh -s -- --key tw_sk_... --api <url>
# Safe to re-run: it updates the binary and config and restarts the service;
# the API maps the same host to the same server.
set -eu

API=""
KEY=""
BIN_URL=""

die() { echo "trailway install: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --key) [ $# -ge 2 ] || die "--key needs a value"; KEY="$2"; shift 2 ;;
    --api) [ $# -ge 2 ] || die "--api needs a value"; API="$2"; shift 2 ;;
    --bin-url) [ $# -ge 2 ] || die "--bin-url needs a value"; BIN_URL="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$KEY" ] || die "missing --key"
[ -n "$API" ] || die "missing --api"
API="${API%/}"
case "$KEY" in tw_sk_*) ;; *) die "key must start with tw_sk_" ;; esac
case "$KEY" in *[!A-Za-z0-9_]*) die "key has invalid characters" ;; esac
case "$API" in http://*|https://*) ;; *) die "--api must start with http:// or https://" ;; esac
case "$API" in *[!A-Za-z0-9._:/@-]*) die "--api has invalid characters" ;; esac

[ "$(uname -s)" = "Linux" ] || die "Linux only"
[ "$(id -u)" -eq 0 ] || die "run as root (use sudo sh)"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac
[ -n "$BIN_URL" ] || BIN_URL="$API/downloads/trailway-agent-linux-$ARCH"

BIN=/usr/local/bin/trailway-agent
CONF_DIR=/etc/trailway
UNIT=/etc/systemd/system/trailway-agent.service

echo "Downloading agent from $BIN_URL"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$BIN_URL" -o "$TMP"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$TMP" "$BIN_URL"
else
  die "curl or wget is required"
fi
install -m 0755 "$TMP" "$BIN"

mkdir -p "$CONF_DIR"
chmod 0700 "$CONF_DIR"
umask 077
printf '{"api":"%s","key":"%s"}\n' "$API" "$KEY" > "$CONF_DIR/agent.json"

cat > "$UNIT" <<UNIT
[Unit]
Description=Trailway agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$BIN run --config $CONF_DIR/agent.json --state /var/lib/trailway/state.json
StateDirectory=trailway
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
chmod 0644 "$UNIT"

systemctl daemon-reload
systemctl enable trailway-agent.service
systemctl restart trailway-agent.service

sleep 2
if systemctl is-active --quiet trailway-agent.service; then
  echo "Trailway agent installed and running."
  echo "Logs: journalctl -u trailway-agent -f"
else
  journalctl -u trailway-agent -n 20 --no-pager >&2 || true
  die "service failed to start"
fi
