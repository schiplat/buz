# syntax=docker/dockerfile:1
# Runtime layout (binary + config only):
#   /app/buz/buz-manager
#   /app/buz/conf/manager.toml
#   /app/buz/data  /app/buz/pipelines

ARG NODE_VERSION=20

FROM node:${NODE_VERSION}-bookworm-slim AS dashboard
WORKDIR /src/dashboard
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci
COPY dashboard/ ./
RUN npm run build

FROM rust:bookworm AS builder
WORKDIR /src
RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=dashboard /src/dashboard/dist ./dashboard/dist

RUN cargo build --locked --release -p manager \
 && strip target/release/buz-manager

FROM debian:bookworm-slim AS runtime
RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app/buz --shell /usr/sbin/nologin buz \
 && mkdir -p /app/buz/conf /app/buz/data /app/buz/pipelines \
 && chown -R buz:buz /app/buz

COPY --from=builder /src/target/release/buz-manager /app/buz/buz-manager
COPY deploy/conf/manager.toml /app/buz/conf/manager.toml

USER buz
WORKDIR /app/buz
EXPOSE 8080
VOLUME ["/app/buz/data", "/app/buz/pipelines"]

ENTRYPOINT ["/app/buz/buz-manager"]
CMD ["-c", "/app/buz/conf/manager.toml"]
