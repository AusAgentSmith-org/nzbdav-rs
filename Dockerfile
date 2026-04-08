# Build stage
FROM rust:1-bookworm AS builder
WORKDIR /build

# Copy workspace manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/nzbdav-core/Cargo.toml crates/nzbdav-core/Cargo.toml
COPY crates/nzbdav-dav/Cargo.toml crates/nzbdav-dav/Cargo.toml
COPY crates/nzbdav-stream/Cargo.toml crates/nzbdav-stream/Cargo.toml
COPY crates/nzbdav-rar/Cargo.toml crates/nzbdav-rar/Cargo.toml
COPY crates/nzbdav-pipeline/Cargo.toml crates/nzbdav-pipeline/Cargo.toml
COPY crates/nzbdav-arr/Cargo.toml crates/nzbdav-arr/Cargo.toml
COPY crates/nzbdav-app/Cargo.toml crates/nzbdav-app/Cargo.toml

# Create dummy source files for dependency caching
RUN for d in core dav stream rar pipeline arr app; do \
      mkdir -p crates/nzbdav-$d/src && \
      echo "" > crates/nzbdav-$d/src/lib.rs; \
    done && \
    echo "fn main() {}" > crates/nzbdav-app/src/main.rs

# Fetch dependencies (all from crates.io)
RUN cargo fetch

# Copy actual source and frontend
COPY crates/ crates/
COPY frontend/ frontend/

# Build release binary
RUN cargo build --release -p nzbdav-app

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/nzbdav-app /usr/local/bin/nzbdav-app

RUN mkdir -p /data
WORKDIR /data

ENV NZBDAV_DB=/data/nzbdav.db
ENV NZBDAV_HOST=0.0.0.0
ENV NZBDAV_PORT=8080
ENV NZBDAV_LOG=info

EXPOSE 8080

ENTRYPOINT ["nzbdav-app"]
