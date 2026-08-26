FROM rust:1.97-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release --bin cfgd

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash git openssh-client ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*

# age, sops and cosign are third-party binaries this image runs the secret and
# module-signing suites through, so each is pinned to a version AND to the
# digest that version publishes: an unverified binary in a test image is a
# supply-chain hole that reaches whatever the suite signs. Every fetch retries
# a transient network fault rather than failing the whole image build on one —
# a bare `curl` timing out at 28 took the E2E infrastructure job down with it.
ARG AGE_VERSION=v1.2.0
ARG AGE_SHA256=2ae71cb3ea761118937a944083f057cfd42f0ef11d197ce72fc2b8780d50c4ef
RUN mkdir -p /usr/local/src \
    && curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 \
    -o /usr/local/src/age.tar.gz \
    "https://github.com/FiloSottile/age/releases/download/${AGE_VERSION}/age-${AGE_VERSION}-linux-amd64.tar.gz" \
    && echo "${AGE_SHA256}  /usr/local/src/age.tar.gz" | sha256sum -c - \
    && tar xzf /usr/local/src/age.tar.gz -C /usr/local/bin --strip-components=1 age/age age/age-keygen \
    && rm -f /usr/local/src/age.tar.gz

ARG SOPS_VERSION=v3.9.4
ARG SOPS_SHA256=5488e32bc471de7982ad895dd054bbab3ab91c417a118426134551e9626e4e85
RUN curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 \
    -o /usr/local/bin/sops \
    "https://github.com/getsops/sops/releases/download/${SOPS_VERSION}/sops-${SOPS_VERSION}.linux.amd64" \
    && echo "${SOPS_SHA256}  /usr/local/bin/sops" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/sops

ARG COSIGN_VERSION=v2.4.3
ARG COSIGN_SHA256=caaad125acef1cb81d58dcdc454a1e429d09a750d1e9e2b3ed1aed8964454708
RUN curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 \
    -o /usr/local/bin/cosign \
    "https://github.com/sigstore/cosign/releases/download/${COSIGN_VERSION}/cosign-linux-amd64" \
    && echo "${COSIGN_SHA256}  /usr/local/bin/cosign" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/cosign

COPY --from=builder /src/target/release/cfgd /usr/local/bin/cfgd
COPY tests/e2e/ /tests/e2e/

# Git identity for tests that init repos
RUN git config --global user.email "e2e@test" && git config --global user.name "E2E"

WORKDIR /tests
ENV NO_COLOR=1
ENV EDITOR=true

ENTRYPOINT ["bash", "/tests/e2e/cli/scripts/run-all.sh"]
