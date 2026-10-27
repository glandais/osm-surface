# syntax=docker/dockerfile:1

# --- build ---------------------------------------------------------------------------------
# Pinned (Rust and Debian release) so that the build is reproducible.
FROM rust:1.99.1-slim-bookworm AS build
# cmake and clang: aws-lc-sys (the rustls crypto provider) builds C code
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Dependencies first, in a layer of their own.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
 && echo 'fn main() {}' > src/main.rs \
 && for b in eval inspect; do echo 'fn main() {}' > src/bin/$b.rs; done \
 && touch src/lib.rs \
 && cargo build --release --locked --bin osm-surface \
 && rm -rf src

COPY src ./src
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked --bin osm-surface \
 && mkdir -p /data/tiles

# --- runtime -------------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/osm-surface /usr/local/bin/osm-surface
COPY --from=build --chown=65532:65532 /data /data
ENV LISTEN_ADDR=0.0.0.0:8002 \
    CACHE_DIR=/data/tiles \
    RUST_LOG=info
VOLUME ["/data"]
EXPOSE 8002
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/osm-surface"]
