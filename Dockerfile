FROM rust:1.96-slim-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.71
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config perl make git \
    && rm -rf /var/lib/apt/lists/*
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin cfgd
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# cfgd-core's generate module embeds these repo-root manifests via include_str!.
COPY examples/ examples/
RUN cargo build --release --bin cfgd

# --- Runtime ---

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git openssh-client curl \
    kmod apparmor-utils procps \
    && rm -rf /var/lib/apt/lists/*

RUN adduser --disabled-password --gecos "" --uid 1000 cfgd

COPY --from=builder /build/target/release/cfgd /usr/local/bin/cfgd

USER cfgd
WORKDIR /home/cfgd

ENTRYPOINT ["cfgd"]
CMD ["--help"]
