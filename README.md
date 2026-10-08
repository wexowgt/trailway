# Trailway

Trailway is a Railway-like deploy platform on a shared server pool. You add your own Linux servers by installing an agent with a key, then deploy Docker images or git repos to them as Firecracker microVMs from a web UI. You always keep full access to your own servers.

Milestone 1 covers own servers only: agent, API, web UI, deploys and metering. Lending idle capacity to the pool comes in milestone 2.

## Layout

- `crates/trailway-api`: control plane API (axum, sqlx, Postgres)
- `crates/trailway-agent`: agent that runs on a user's server
- `crates/trailway-proto`: shared serde types for API <-> agent messages
- `web/`: Next.js (App Router, TypeScript) web UI

## Run locally

Requires Rust (stable), Node 22+, pnpm and Docker.

```sh
cp .env.example .env
docker compose up -d                 # Postgres 16 on :5432
set -a; source .env; set +a
cargo run -p trailway-api            # applies migrations, serves on 127.0.0.1:8080
curl localhost:8080/healthz          # {"status":"ok"}

cargo run -p trailway-agent          # prints its version

cd web && pnpm install && pnpm dev   # http://localhost:3000
```

## Adding a server (agent)

A user creates a server key (`POST /api/v1/server-keys`), then on the Linux box:

```sh
curl -fsSL <api>/install.sh | sudo sh -s -- --key tw_sk_... --api <api>
```

The script downloads `<api>/downloads/trailway-agent-linux-<arch>` (or `--bin-url`), writes `/etc/trailway/agent.json` (0600), installs `trailway-agent.service` and enables it (so it survives reboot). The agent calls `POST /api/v1/agent/register` with the key, stores the returned server token in `/var/lib/trailway/state.json`, then sends `POST /api/v1/agent/heartbeat` every 10 s (CPU millicores, RAM and disk bytes, KVM). No heartbeat for 30 s means offline. Re-running the installer (or re-registering the same `/etc/machine-id` for the same user) updates the existing server and rotates its token. `GET /api/v1/servers` lists the caller's servers.

The API serves binaries from `API_AGENT_DIST_DIR` (files `trailway-agent-linux-x86_64` and `-aarch64`; build with `cargo zigbuild --release --target x86_64-unknown-linux-gnu -p trailway-agent`).

Reaching a local API from a remote test box: `ssh -R 18080:127.0.0.1:18080 root@<host>`, then install with `--api http://127.0.0.1:18080` on the box. `TRAILWAY_MACHINE_ID` overrides the machine id when running the agent on a host without `/etc/machine-id` (macOS dev).

## Deploying a service

All routes need the session cookie. Project -> environments -> services -> deployments:

```sh
POST /api/v1/projects {"name"}                       GET/PATCH/DELETE /api/v1/projects/{id}
POST /api/v1/projects/{id}/environments {"name"}     GET/PATCH/DELETE /api/v1/environments/{id}
POST /api/v1/environments/{id}/services              GET/PATCH/DELETE /api/v1/services/{id}
     {"name","image","vcpus","memory_mib","env":{},"port","server_id"}
POST /api/v1/services/{id}/deploy                    202 + deployment (queued)
POST /api/v1/services/{id}/stop                      removes the VM, keeps the service
GET  /api/v1/services/{id}/deployments               GET /api/v1/deployments/{id}
GET  /api/v1/deployments/{id}/logs[?follow=true]     console output as text
```

`server_id` must be one of the caller's servers. A deploy is refused with 409 `insufficient_capacity` when the server's free CPU or memory (from the last heartbeat, plus the service's own VM that is being replaced, minus deploys still on their way) is too small, `server_offline` without a recent heartbeat and `kvm_unavailable` without `/dev/kvm`. Config changes (PATCH) apply on the next deploy, which stops the old VM and starts the new one. Deleting a service, environment or project stops its VMs first.

The agent keeps one outbound WebSocket (`GET /api/v1/agent/ws`, server token) so it works behind NAT. The API sends `deploy` and `stop` jobs; the agent reports `queued`, `building` (image pull), `deploying`, `running` (with the forwarded host port), `failed` or `stopped`, and streams the console log in chunks keyed by byte offset. After every (re)connect the agent sends `hello` with the actual state of each deployment it knows (kept in `deployments.json` next to `state.json`) and the API reconciles: lost jobs are handed over again, VMs that are gone are marked failed, unwanted ones are stopped. Types are in `crates/trailway-proto`. `TRAILWAY_FAKE_RUNTIME=1` runs the agent with simulated VMs (no KVM needed) for local development.

### Public HTTPS URL

A service with a `port` gets `url` in the service API, like `https://hello-production.178-104-208-91.sslip.io` (`<service>-<environment>.<server ip with dashes>.sslip.io`). The label is fixed when the service is created, so redeploys and renames keep the URL; a second service with the same label on a server gets a short id suffix. The agent reports its public IP in every heartbeat (`TRAILWAY_PUBLIC_IP` overrides the lookup), so the URL appears once the server has sent one.

Each host runs Caddy (installed by the installer and `scripts/setup-host.sh`; ports 80 and 443 must be open). The agent owns one Caddy HTTP server and keeps a route per running service in it through Caddy's admin API (`127.0.0.1:2019`, `TRAILWAY_CADDY_ADMIN`): the route points at the VM's forwarded host port, is swapped in place on redeploy and removed on stop. Caddy gets the Let's Encrypt certificate on its own and proxies HTTP and WebSockets. The routes are re-synced every 30 s, so a Caddy restart heals itself. `TRAILWAY_PROXY=off` disables this (development without Caddy).

The API's `API_DOMAIN_BASE` (default `{ip}.sslip.io`, `{ip}` being the dashed server IP) sets the domain: use a wildcard domain such as `apps.example.com` with `*.apps.example.com` pointing at the servers once there is one.

## Checks (same as CI)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd web && pnpm lint && pnpm build
```

## Running a microVM (agent)

On a Linux host with KVM: `sudo scripts/setup-host.sh`, then
`trailway-agent vm run --image nginxdemos/hello --vcpus 1 --mem 256`,
`trailway-agent vm status <id>`, `trailway-agent vm stop <id>`.
Paths are overridable via `TRAILWAY_DATA_DIR`, `TRAILWAY_KERNEL`,
`TRAILWAY_FIRECRACKER`, `TRAILWAY_BUSYBOX`. Rootfs images are cached per
image digest under `$TRAILWAY_DATA_DIR/images/`.

## Ledger

Every 60 s the agent sends a usage sample (`POST /api/v1/agent/usage`, averaged from its heartbeats: total and used CPU/RAM). The API credits the idle part (`total - used`) to the server owner as an append-only `contributed` ledger entry, in vCPU-seconds and GB-seconds (1 GB = 2^30 bytes), kept separate. Offline servers, servers without KVM, stale samples and duplicate or overlapping samples earn nothing. `GET /api/v1/ledger/balance`, `GET /api/v1/ledger/entries?limit=&before=` and `GET /api/v1/servers/{id}/usage?from=&to=` read it back. Borrowing (`consumed` entries) is not implemented yet.

## Production

The control plane runs on one host (test box `178.104.208.91`) at `https://trailway.178-104-208-91.sslip.io`, next to the agent, Caddy and other sites served by nginx.

- **Stack**: `docker-compose.prod.yml` in `/opt/trailway` runs Postgres 16 (volume `pgdata`), the API and the web UI, all with `restart: unless-stopped`, so data and containers survive `down && up -d` and a reboot. Ports bind to `127.0.0.1` only (API `:8080`, web `:3000`). The API image also builds the Linux agent and serves it from `/downloads`.
- **Front (nginx owns 80 and 443 on this host)**:
  - nginx's `stream` module splits public `:443` by TLS server name without decrypting (`deploy/nginx/stream.conf.tmpl`). `*.178-104-208-91.sslip.io` service hosts go to the agent's Caddy on `127.0.0.1:8443`; the control plane and every other site go to nginx's vhosts on `127.0.0.1:4443`.
  - The control plane vhost (`deploy/nginx/trailway.conf.tmpl`, certbot certificate) sends `/api/*`, `/install.sh` and `/downloads/*` to the API and everything else to the web UI, one origin, so the `Secure` session cookie works (`API_COOKIE_SECURE=true`). `GET /api/healthz` answers from the API.
  - Caddy keeps only its admin API in `/etc/caddy/Caddyfile`. The agent runs with `TRAILWAY_CADDY_LISTEN=127.0.0.1:8443` (systemd drop-in), creates its server there and only touches routes with `@id` `tw-*`, re-syncing every 30 s. A Caddy restart loses nothing, the agent recreates the server on its next sync. On a host where Caddy owns 80/443 leave the variable unset (default `:443`).
- **First setup** (once, as root on the host, after `scripts/setup-host.sh`):
  1. `CONTROL_DOMAIN=trailway.178-104-208-91.sslip.io scripts/setup-control-plane.sh` installs Docker, creates `/opt/trailway/.env` (random Postgres password, `IMAGE_PREFIX`, `IMAGE_TAG`) and the nightly backup.
  2. `CONTROL_DOMAIN=... scripts/setup-nginx-front.sh` edits live nginx config. It backs up `/etc/nginx` to `/root/nginx-backup-*.tar.gz` first, restores it if `nginx -t` fails, moves existing `listen 443` lines to `127.0.0.1:4443`, adds the stream split and the vhost, and issues the certificate.
- **Deploys**: `.github/workflows/deploy.yml` runs after CI passes on `main`. It builds both images, pushes them to GHCR (`ghcr.io/<owner>/trailway-{api,web}`, tagged with the commit SHA and `latest`), then SSHes to the host, logs Docker in to GHCR, and runs `compose pull && up -d`. Secret: `DEPLOY_SSH_KEY` (private key authorised for `root` on the host); optional repo variable `DEPLOY_HOST`. Create it with `ssh-keygen -t ed25519 -N '' -f deploy_key`, append `deploy_key.pub` to `/root/.ssh/authorized_keys`, then `gh secret set DEPLOY_SSH_KEY < deploy_key` (never paste the private key anywhere else).
- **Backups**: `/etc/cron.d/trailway-backup` runs `trailway-backup` at 03:15 and writes `/var/backups/trailway/trailway-<timestamp>.sql.gz`, keeping the last 7. Take one now: `COMPOSE_DIR=/opt/trailway trailway-backup`.
- **Restore**: `cd /opt/trailway && docker compose -f docker-compose.prod.yml stop api web && docker compose -f docker-compose.prod.yml exec -T postgres psql -U trailway -c 'DROP DATABASE trailway' -c 'CREATE DATABASE trailway' postgres && gunzip -c /var/backups/trailway/<file>.sql.gz | docker compose -f docker-compose.prod.yml exec -T postgres psql -U trailway trailway && docker compose -f docker-compose.prod.yml start api web`.
- **Swap the domain**: point DNS at the host, run `CONTROL_DOMAIN=<new domain> SERVICE_SUFFIX=<service base domain> scripts/setup-nginx-front.sh`, set `API_DOMAIN_BASE` for the API, then reinstall the agent with `--api https://<new domain>`. Nothing in the code names the domain.
- **Agent on the box**: `curl -fsSL https://trailway.178-104-208-91.sslip.io/install.sh | sh -s -- --key tw_sk_... --api https://trailway.178-104-208-91.sslip.io`, no tunnel needed. Re-run `scripts/setup-nginx-front.sh` or restart `trailway-agent` afterwards if the installer rewrote the unit (the drop-in survives).
