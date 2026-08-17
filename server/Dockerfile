# --- Stage 1: build ---
# Using the full rust image here (not slim) because compiling the
# workspace needs a real toolchain; this stage's size doesn't matter,
# it never ships.
FROM rust:1.82 AS builder

WORKDIR /app

# Copy manifests first and build a throwaway "dummy" source tree so
# Docker's layer cache holds onto the compiled dependencies between
# builds -- same idea as caching a pip/conda layer separately from
# your actual pipeline code.
COPY Cargo.toml ./
COPY core/Cargo.toml core/Cargo.toml
COPY protocol/Cargo.toml protocol/Cargo.toml
COPY client/Cargo.toml client/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml

RUN mkdir -p core/src protocol/src client/src server/src \
    && echo "fn main() {}" > server/src/main.rs \
    && echo "" > core/src/lib.rs \
    && echo "" > protocol/src/lib.rs \
    && echo "fn main() {}" > client/src/main.rs

# Only build the server's dependency graph -- we never need to compile
# bevy's render stack (client) inside this image at all.
RUN cargo build --release -p game_server || true

# Now copy the real source and force a rebuild of just our own crates
# (the dependency layer above is already cached).
COPY core core
COPY protocol protocol
COPY server server
RUN touch core/src/lib.rs protocol/src/lib.rs server/src/main.rs \
    && cargo build --release -p game_server

# --- Stage 2: runtime ---
# Debian slim has just enough libc for a Bevy MinimalPlugins binary.
# No X11, no GPU drivers, no audio libs -- none of that got compiled
# in to begin with, since server/Cargo.toml uses default-features=false.
FROM debian:bookworm-slim AS runtime

RUN useradd -m -u 1000 gameserver
USER gameserver
WORKDIR /app

COPY --from=builder /app/target/release/game_server /app/game_server

EXPOSE 5000/udp
ENTRYPOINT ["/app/game_server"]
