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

## Conventions

- CI (`.github/workflows/ci.yml`) must stay green: fmt, clippy `-D warnings`, tests, web lint and build.
- Add dependencies via `[workspace.dependencies]` in the root `Cargo.toml`.
- New sqlx migrations go in `crates/trailway-api/migrations/` as `NNNN_name.sql`.
- Tests that need Postgres must not break plain `cargo test` in CI (no DB service there).
- GitHub repo is wexowgt/trailway: use `gh`, not `glab`. PRs target `main`.
- No em dashes in docs or commits.
