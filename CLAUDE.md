# Trailway

Railway-like deploy platform on a shared server pool. See README.md for the product overview.

## Layout

- `crates/trailway-api`: axum + sqlx + Postgres control plane. `src/lib.rs` holds the router (testable with `tower::ServiceExt::oneshot`), `src/main.rs` connects to Postgres, runs `migrations/` and serves. Config via env: `DATABASE_URL`, `API_BIND`.
- `crates/trailway-agent`: binary that runs on the user's Linux server (will drive Firecracker microVMs).
- `crates/trailway-proto`: serde types shared by API and agent. Put every wire type here.
- `web/`: Next.js App Router, TypeScript, pnpm. Dark theme tokens are CSS variables in `app/globals.css`.

## Commands

- `docker compose up -d`: local Postgres 16 (creds in `.env.example`)
- `cargo run -p trailway-api` / `cargo run -p trailway-agent`
- `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- `cd web && pnpm dev | pnpm lint | pnpm build`

## Web and API

- The browser only talks to the Next origin. `web/next.config.ts` rewrites `/api/v1/*`, `/install.sh` and `/downloads/*` to the Rust API (`API_URL`, default `http://127.0.0.1:8080`), so the `tw_session` cookie is same-origin and the install command uses `window.location.origin`.
- `web/middleware.ts` redirects to `/login` without a session cookie; `web/app/(app)/layout.tsx` validates it with `GET /api/v1/me`.
- Servers page polls `/servers` every 5s. Local run: Postgres, `cargo run -p trailway-api`, `cd web && pnpm dev`.

## Public URLs

- `crates/trailway-agent/src/proxy.rs` keeps Caddy routes (admin API) in sync with running deployments; `deploy::Manager::sync_routes` derives them. The API computes the domain (`crates/trailway-api/src/domains.rs`, base from `API_DOMAIN_BASE`) and sends it in the deploy job; `services.host_label` keeps the URL stable.

## Conventions

- CI (`.github/workflows/ci.yml`) must stay green: fmt, clippy `-D warnings`, tests, web lint and build.
- Add dependencies via `[workspace.dependencies]` in the root `Cargo.toml`.
- New sqlx migrations go in `crates/trailway-api/migrations/` as `NNNN_name.sql`.
- Tests that need Postgres must not break plain `cargo test` in CI (no DB service there).
- GitHub repo is wexowgt/trailway: use `gh`, not `glab`. PRs target `main`.
- No em dashes in docs or commits.
