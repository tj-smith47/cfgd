#!/usr/bin/env bash
# Build the statically-linked cfgd the demo image copies in, at demo/bin/cfgd.
#
# Kept here rather than inline in the Taskfile so a change to how the recorded
# binary is built is a change under demo/scripts, which check-sync.sh counts as
# a render input.
set -euo pipefail

cd "$(dirname "$0")/../.."

# musl, not the default gnu target: the demo image is Ubuntu 24.04 (glibc
# 2.39) and this host is newer, so a dynamically-linked build refuses to
# start in the container. Same artifact the release pipeline ships.
#
# zigbuild, not plain cargo build: ring compiles C, and there is no
# x86_64-linux-musl-gcc on this host; zig supplies the cross C toolchain,
# exactly as the nightly cross-compile job does.
cargo zigbuild --release --target x86_64-unknown-linux-musl --bin cfgd

# `install -D` is a GNU coreutils extension; BSD/macOS `install` lacks it, so
# create the destination directory separately and use the `-m`-only form both
# userlands support.
mkdir -p demo/bin
install -m 0755 target/x86_64-unknown-linux-musl/release/cfgd demo/bin/cfgd
