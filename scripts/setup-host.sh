#!/usr/bin/env bash
# Installs everything trailway-agent needs to run Firecracker microVMs on a
# fresh Ubuntu/Debian host. Safe to re-run. Run as root.
set -euo pipefail

FC_VERSION="${FC_VERSION:-v1.10.1}"
KERNEL_URL="${KERNEL_URL:-https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/$(uname -m)/vmlinux-5.10.225}"
DATA_DIR="${TRAILWAY_DATA_DIR:-/var/lib/trailway}"
ARCH="$(uname -m)"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }

if [ ! -e /dev/kvm ]; then
  echo "ERROR: /dev/kvm is missing. Enable virtualization (nested virt on a VPS) and retry." >&2
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl ca-certificates e2fsprogs busybox-static skopeo umoci iproute2 nftables

# Caddy terminates HTTPS (Let's Encrypt) for service URLs; the agent adds and
# removes routes through its admin API on 127.0.0.1:2019.
install_caddy() {
  if ! command -v caddy >/dev/null 2>&1; then
    if ! command -v apt-get >/dev/null 2>&1; then
      echo "WARNING: not a Debian/Ubuntu host, install Caddy yourself for public service URLs." >&2
      return 0
    fi
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key | gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list
    apt-get update -qq
    apt-get install -y -qq caddy
  fi
  mkdir -p /etc/caddy
  # Routes are not in a file: the agent creates the HTTP server at runtime.
  printf '{\n\tadmin 127.0.0.1:2019\n}\n' > /etc/caddy/Caddyfile.trailway
  if ! cmp -s /etc/caddy/Caddyfile.trailway /etc/caddy/Caddyfile || ! systemctl is-active --quiet caddy; then
    mv /etc/caddy/Caddyfile.trailway /etc/caddy/Caddyfile
    systemctl enable caddy >/dev/null 2>&1 || true
    systemctl restart caddy
  fi
  rm -f /etc/caddy/Caddyfile.trailway
  if command -v ufw >/dev/null 2>&1 && ufw status | grep -q "Status: active"; then
    ufw allow 80/tcp >/dev/null
    ufw allow 443/tcp >/dev/null
  fi
}

if ! command -v firecracker >/dev/null || ! firecracker --version | grep -q "${FC_VERSION#v}"; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/${FC_VERSION}/firecracker-${FC_VERSION}-${ARCH}.tgz" | tar -xz -C "$tmp"
  install -m 0755 "$tmp/release-${FC_VERSION}-${ARCH}/firecracker-${FC_VERSION}-${ARCH}" /usr/local/bin/firecracker
fi

mkdir -p "$DATA_DIR"
if [ ! -s "$DATA_DIR/vmlinux" ]; then
  curl -fsSL -o "$DATA_DIR/vmlinux.part" "$KERNEL_URL"
  mv "$DATA_DIR/vmlinux.part" "$DATA_DIR/vmlinux"
fi

install_caddy

chmod a+rw /dev/kvm 2>/dev/null || true
echo "firecracker: $(firecracker --version | head -1)"
echo "kernel:      $DATA_DIR/vmlinux"
echo "caddy:      $(command -v caddy || echo missing) (ports 80 and 443 must be reachable from the internet)"
echo "busybox:     $(command -v busybox) (set TRAILWAY_BUSYBOX if not /bin/busybox)"
