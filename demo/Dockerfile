# Pinned (not `latest`): the demo depends on this distro's glibc (2.39, the
# floor above which Homebrew stops installing its own copy) and on apt's Ruby
# (3.2, above solargraph's 3.1 floor). A floating tag can land a point release
# that regresses either, and the resulting take fails halfway through the
# recording rather than at the start of it. Nothing here tracks Homebrew's
# formula index, which a pin would go stale against — this image ships none of
# Homebrew at all; cfgd bootstraps it live during the recorded apply.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# ubuntu:24.04 ships a default `ubuntu` user at uid 1000 / gid 1000, which
# collides with the uid this image needs `tj` to hold — WORKDIR, the final
# COPY --chown, and USER all name that account. Remove it before creating tj.
RUN userdel -r ubuntu

# None of this ships in a bare ubuntu:24.04, and each entry maps to one
# requirement: sudo (cfgd's apt provider shells out to `sudo apt-get`),
# git/curl/ca-certificates (the tape's HTTPS clone), build-essential/
# procps/file (Homebrew's own documented Linux prerequisites), gnupg (the
# source-trust tape's `git log --format=%G?` signature check, and the key
# generation + `git commit -S` that satisfies it). nvim is deliberately
# absent — the demo's first beat is `nvim` failing with "command not
# found" — and none of these packages pull in Ubuntu's command-not-found
# handler, so that failure stays the plain bash message.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       sudo \
       git \
       curl \
       ca-certificates \
       build-essential \
       procps \
       file \
       gnupg \
    && rm -rf /var/lib/apt/lists/*

# glibc has shipped the portable `C.utf8` locale (no `locales` package or
# localedef needed to generate it) since 2.35, well below this image's 2.39 —
# enough UTF-8 encoding for LazyVim's Nerd Font glyphs and brew's output.
ENV LANG=C.utf8 \
    LANGUAGE=C.utf8 \
    LC_ALL=C.utf8

# An ordinary user, not `linuxbrew`: the demo's claim is that cfgd bootstraps
# Homebrew onto a bare machine, and starting the container already logged in
# as `linuxbrew` — the account Homebrew's own installer creates when it runs
# AS ROOT — would make it look pre-provisioned before cfgd runs a command. A
# non-root `tj` takes cfgd's brew provider down its non-root install path
# instead, installing straight into `tj`'s own home with no system user
# created at all. `tj` still needs passwordless sudo, for cfgd's apt
# provider (`sudo apt-get`), codified up front.
RUN useradd --uid 1000 --create-home --shell /bin/bash --user-group tj \
    && echo "tj ALL=(ALL) NOPASSWD: ALL" | tee /etc/sudoers.d/tj-nopasswd > /dev/null \
    && chmod 0440 /etc/sudoers.d/tj-nopasswd \
    && visudo -cf /etc/sudoers.d/tj-nopasswd

USER tj
WORKDIR /home/tj

# PATH is deliberately NOT baked here, and neither is any HOMEBREW_* value —
# both were the Homebrew-derived base image's job, not this one's. When cfgd
# bootstraps Homebrew it records the PATH directories that install
# contributed, then writes them into ~/.cfgd.env and injects the source line
# into ~/.bashrc — so `nvim` resolving after the apply is the product
# working, and the brew prefix existing at all, not the image pre-arranging
# either outcome.

# The image pins one cfgd build forever, so its update check can only ever say
# "a newer cfgd exists" — an upgrade nag on camera that has nothing to do with
# what the recording demonstrates.
ENV CFGD_NO_UPDATE_CHECK=1

# `docker run -t` forwards TERM and nothing else, so COLORTERM never reaches the
# container and cfgd quantizes its theme to the 256-colour cube — a visibly
# duller dracula than the terminal recording this can actually display. The
# capability is the recording terminal's, and it is 24-bit.
ENV COLORTERM=truecolor

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
# on tj keeps `WORKDIR /home/tj/cfgd` (which BuildKit creates lazily, owned
# by whichever USER is active) tj-owned instead of root-owned.
COPY --chmod=0755 demo/bin/cfgd /usr/local/bin/cfgd

WORKDIR /home/tj/cfgd

# Real source tree for `nvim .` to open on both sides of the demo. .dockerignore
# is an allowlist — it ignores `*` and re-admits only what a cfgd-core build
# and the demo binary actually need (see .dockerignore for the exact list, not
# duplicated here so this comment can't drift out of sync with it) — so
# target/ (gigabytes of build output the demo has no use for) never reaches
# the context in the first place. Last layer in the file, so editing the repo
# never invalidates the apt and user-setup layers above it.
COPY --chown=tj:tj . .
