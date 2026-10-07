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

## Checks (same as CI)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd web && pnpm lint && pnpm build
```
