# Pinned (not `latest`): the demo depends on this distro's glibc (2.39, the
# floor above which Homebrew stops installing its own copy) and apt's Ruby
# (3.2, above solargraph's 3.1 floor) — a floating tag could land a point
# release that regresses either and silently reintroduce the bug this rebase
# fixes. The old base was pinned to `latest` for the opposite reason: it
# tracked Homebrew's own evolving formula index, which a pinned digest would
# go stale against immediately. This image ships none of Homebrew at all —
# cfgd bootstraps it live during the recorded apply — so only Ubuntu's
# packages and glibc need to stay put.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# ubuntu:24.04 ships a default `ubuntu` user at uid 1000 / gid 1000, which
# collides with the uid this image needs `linuxbrew` to hold — WORKDIR, the
# final COPY --chown, and USER all name that account. Remove it before
# creating linuxbrew.
RUN userdel -r ubuntu

# None of this ships in a bare ubuntu:24.04, and each entry maps to one
# requirement: sudo (cfgd's apt provider shells out to `sudo apt-get`),
# git/curl/ca-certificates (the tape's HTTPS clone), build-essential/
# procps/file (Homebrew's own documented Linux prerequisites). nvim is
# deliberately absent — the demo's first beat is `nvim .` failing with
# "command not found" — and none of these packages pull in Ubuntu's
# command-not-found handler, so that failure stays the plain bash message.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       sudo \
       git \
       curl \
       ca-certificates \
       build-essential \
       procps \
       file \
    && rm -rf /var/lib/apt/lists/*

# glibc has shipped the portable `C.utf8` locale (no `locales` package or
# localedef needed to generate it) since 2.35, well below this image's 2.39 —
# enough UTF-8 encoding for LazyVim's Nerd Font glyphs and brew's output.
ENV LANG=C.utf8 \
    LANGUAGE=C.utf8 \
    LC_ALL=C.utf8

# Homebrew refuses to run as root, so linuxbrew needs passwordless sudo
# (for cfgd's apt provider) codified up front, before it ever runs a command.
RUN useradd --uid 1000 --create-home --shell /bin/bash --user-group linuxbrew \
    && echo "linuxbrew ALL=(ALL) NOPASSWD: ALL" | tee /etc/sudoers.d/linuxbrew-nopasswd > /dev/null \
    && chmod 0440 /etc/sudoers.d/linuxbrew-nopasswd \
    && visudo -cf /etc/sudoers.d/linuxbrew-nopasswd

USER linuxbrew
WORKDIR /home/linuxbrew

# Baked into the image config rather than a shell rc file so every process
# the container starts inherits it — including the tape's `bash -l` login
# shell, which must resolve `nvim` after the apply. No brew exists at build
# time to have written this itself, and cfgd's own bootstrap never writes
# `brew shellenv` into any rc file either — this baked PATH is the only
# thing that makes `nvim` resolve once the apply installs it.
ENV PATH="/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:${PATH}" \
    HOMEBREW_PREFIX=/home/linuxbrew/.linuxbrew \
    HOMEBREW_CELLAR=/home/linuxbrew/.linuxbrew/Cellar \
    HOMEBREW_REPOSITORY=/home/linuxbrew/.linuxbrew/Homebrew

# cfgd enters as a prebuilt host binary, never built in-container: building
# Rust here would add a toolchain and minutes of compile time the demo has no
# use for. Staged at demo/bin/cfgd before `docker build` runs (see
# .dockerignore — target/ itself never enters the build context).
#
# Built for x86_64-unknown-linux-musl, not the default gnu target: this
# image's glibc (Ubuntu 24.04, 2.39) is older than the build host's (Ubuntu
# 26.04, 2.43), and a dynamically-linked gnu binary refuses to run against a
# libc older than the one it was linked against. The musl build is
# statically linked with no glibc dependency at all — the same artifact
# cfgd's own release pipeline already ships in every deb/rpm/apk package
# (.anodizer.yaml build id `cfgd-musl`) for exactly this reason.
#
# No USER root switch for this or the WORKDIR/COPY below: COPY writes
# directly into the build layer regardless of the active user's Unix write
# permission on the target directory, so it needs no elevation, and staying
# on linuxbrew keeps `WORKDIR /home/linuxbrew/cfgd` (which BuildKit creates
# lazily, owned by whichever USER is active) linuxbrew-owned instead of
# root-owned.
COPY --chmod=0755 demo/bin/cfgd /usr/local/bin/cfgd

WORKDIR /home/linuxbrew/cfgd

# Real source tree for `nvim .` to open on both sides of the demo — this
# repo, filtered by the existing .dockerignore (crates/, examples/, tests/,
# Cargo.*, README.md). Never target/, which is gigabytes of build output the
# demo has no use for. Last layer in the file, so editing the repo never
# invalidates the apt and user-setup layers above it.
COPY --chown=linuxbrew:linuxbrew . .
