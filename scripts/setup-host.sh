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

chmod a+rw /dev/kvm 2>/dev/null || true
echo "firecracker: $(firecracker --version | head -1)"
echo "kernel:      $DATA_DIR/vmlinux"
echo "busybox:     $(command -v busybox) (set TRAILWAY_BUSYBOX if not /bin/busybox)"
