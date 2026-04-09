FROM rust:1.88-alpine3.21 AS builder

RUN apk add --no-cache musl-dev build-base openssl-dev openssl-libs-static git

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

# Fetch dependencies (all from crates.io — no private registry)
RUN cargo fetch

# Copy actual source and frontend
COPY crates/ crates/
COPY frontend/ frontend/

# RELEASE_OPTIMIZED=true enables fat LTO + single codegen-unit (slow but smaller binary)
ARG RELEASE_OPTIMIZED=false

RUN if [ "$RELEASE_OPTIMIZED" = "true" ]; then \
      export CARGO_PROFILE_RELEASE_LTO=fat \
             CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
             CARGO_PROFILE_RELEASE_STRIP=symbols; \
    fi && \
    cargo build --release -p nzbdav-app

FROM alpine:3.21

RUN apk add --no-cache ca-certificates curl

COPY --from=builder /build/target/release/nzbdav-app /usr/local/bin/nzbdav-app

RUN mkdir -p /data
WORKDIR /data

ENV NZBDAV_DB=/data/nzbdav.db
ENV NZBDAV_HOST=0.0.0.0
ENV NZBDAV_PORT=8080
ENV NZBDAV_LOG=info

EXPOSE 8080

ENTRYPOINT ["nzbdav-app"]
