# syntax=docker/dockerfile:1
#
# The two hosted binaries, for docker-compose.yml: `game-server` and
# `auth-server` (pick one with `--target`). The game client is never built
# here -- players run it on their own machines.

# --- Build ---
# Same toolchain as development; bookworm to match the runtime images'
# glibc.
FROM rust:1.97-bookworm AS builder
WORKDIR /app
COPY . .
# The cache mounts keep the crate registry and compiled dependencies
# between builds, so a code change only recompiles this workspace's own
# crates. The target dir is a cache, not a layer, hence copying the
# binaries out.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked -p game_server -p auth_server \
    && mkdir /out \
    && cp target/release/game_server target/release/auth_server /out/

# --- Game server ---
# No window, GPU or audio libraries: server/Cargo.toml builds Bevy with
# default-features = false.
FROM debian:bookworm-slim AS game-server
RUN useradd --create-home --uid 1000 game
WORKDIR /app
# Everything it reads at startup: tuning, data files and the map layout
# (the map art under gallery/maps/tiles is only for the client).
COPY config config
COPY data data
COPY gallery/maps/*.ron gallery/maps/
COPY gallery/maps/zones gallery/maps/zones
COPY --from=builder /out/game_server ./
# Character saves, on a volume -- see docker-compose.yml.
RUN mkdir saves && chown game saves
USER game
ENV ARPG_SERVER_ADDR=0.0.0.0:5000
EXPOSE 5000/udp
ENTRYPOINT ["/app/game_server"]

# --- Auth server ---
FROM debian:bookworm-slim AS auth-server
RUN useradd --create-home --uid 1000 auth
WORKDIR /app
COPY --from=builder /out/auth_server ./
# Accounts and sessions, on a volume -- see docker-compose.yml.
RUN mkdir saves && chown auth saves
USER auth
ENV ARPG_AUTH_ADDR=0.0.0.0:5001
EXPOSE 5001/tcp
ENTRYPOINT ["/app/auth_server"]
