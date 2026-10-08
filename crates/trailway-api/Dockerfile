# Build context: repo root. Also builds the Linux agent so the API can serve it
# from /downloads (glibc 2.31 base so it runs on older Ubuntu/Debian hosts).
FROM rust:1-bullseye AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p trailway-api -p trailway-agent

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/trailway-api /usr/local/bin/trailway-api
COPY --from=build /src/target/release/trailway-agent /opt/dist/trailway-agent-linux-x86_64
ENV API_BIND=0.0.0.0:8080 API_AGENT_DIST_DIR=/opt/dist
EXPOSE 8080
CMD ["trailway-api"]
