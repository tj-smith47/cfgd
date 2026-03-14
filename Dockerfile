FROM rust:1.94-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libsqlite3-dev git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release --bin cfgd

# --- Runtime ---

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libsqlite3-0 git openssh-client \
    kmod apparmor-utils procps \
    && rm -rf /var/lib/apt/lists/*

RUN adduser --disabled-password --gecos "" --uid 1000 cfgd

COPY --from=builder /build/target/release/cfgd /usr/local/bin/cfgd

USER cfgd
WORKDIR /home/cfgd

ENTRYPOINT ["cfgd"]
CMD ["--help"]
