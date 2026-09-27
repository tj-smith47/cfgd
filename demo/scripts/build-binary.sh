#!/usr/bin/env bash
# Build the statically-linked cfgd the demo image copies in, at demo/bin/cfgd.
#
# Kept here, outside the Taskfile, so a change to how the recorded
# binary is built is a change under demo/scripts, which check-sync.sh counts as
# a render input.
set -euo pipefail

cd "$(dirname "$0")/../.."

# musl: the demo image is Ubuntu 24.04 (glibc
# 2.39) and this host is newer, so a dynamically-linked build refuses to
# start in the container. Same artifact the release pipeline ships.
#
# zigbuild: ring compiles C, and there is no
# x86_64-linux-musl-gcc on this host; zig supplies the cross C toolchain,
# exactly as the nightly cross-compile job does.
target=x86_64-unknown-linux-musl

# The recorded binary must not carry cfgd-core's test-helpers feature (it swaps
# $HOME for a throwaway test home) or the test-fixtures crate. Cargo decides
# that through default-members, feature forwarding, renamed and inherited
# dependencies, so its own resolved graph is asked, for the same package
# selection and target the build below uses. no-dev: a build does not unify
# dev-dependency features, and the fixtures crate is a dev-dependency.
tree="$(cargo tree -e features,no-dev --target "$target" -f '{p} {f}' --prefix none)"
# Read from a here-string: `grep -q` on a pipe exits at its first match and
# would SIGPIPE a producer under pipefail.
if ! grep -q '^cfgd-core ' <<<"$tree"; then
  echo "build-binary.sh: cargo tree did not name cfgd-core, so it judged nothing" >&2
  exit 1
fi
if refused="$(grep -E 'test-helpers|cfgd-test-fixtures' <<<"$tree")"; then
  echo "build-binary.sh: refusing to build: the demo binary would carry test-only code:" >&2
  sort -u <<<"$refused" >&2
  exit 1
fi

cargo zigbuild --release --target "$target" --bin cfgd

# `install -D` is a GNU coreutils extension; BSD/macOS `install` lacks it, so
# create the destination directory separately and use the `-m`-only form both
# userlands support.
mkdir -p demo/bin
install -m 0755 "target/$target/release/cfgd" demo/bin/cfgd
