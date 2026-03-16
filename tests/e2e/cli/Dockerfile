FROM rust:1.94-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release --bin cfgd

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash git openssh-client ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*

# Install age (for secret tests)
RUN curl -fsSL https://github.com/FiloSottile/age/releases/download/v1.2.0/age-v1.2.0-linux-amd64.tar.gz \
    | tar xz -C /usr/local/bin --strip-components=1 age/age age/age-keygen

# Install sops (for secret tests)
RUN curl -fsSL -o /usr/local/bin/sops \
    https://github.com/getsops/sops/releases/download/v3.9.4/sops-v3.9.4.linux.amd64 \
    && chmod +x /usr/local/bin/sops

COPY --from=builder /src/target/release/cfgd /usr/local/bin/cfgd
COPY tests/e2e/ /tests/e2e/

# Git identity for tests that init repos
RUN git config --global user.email "e2e@test" && git config --global user.name "E2E"

WORKDIR /tests
ENV NO_COLOR=1
ENV EDITOR=true

ENTRYPOINT ["bash", "/tests/e2e/cli/scripts/run-exhaustive-tests.sh"]
