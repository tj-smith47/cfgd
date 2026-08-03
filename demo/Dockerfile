# Homebrew is the only supported install path for the nvim module this demo
# runs (its module.yaml denies apt for neovim outright), so the base image
# must already carry a working brew — homebrew/brew is the only one that does.
# Pinned to `latest` deliberately: this image tracks Homebrew's own evolving
# formula index, which a pinned digest would go stale against immediately.
FROM homebrew/brew:latest

# The base image's default user (linuxbrew, uid 1000) already gets
# passwordless sudo, but that's an upstream default under a mutable `latest`
# tag, not a guarantee. Codify it explicitly so this image doesn't silently
# regress if the base ever changes.
RUN echo "linuxbrew ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/linuxbrew-nopasswd > /dev/null \
    && sudo chmod 0440 /etc/sudoers.d/linuxbrew-nopasswd \
    && sudo visudo -cf /etc/sudoers.d/linuxbrew-nopasswd

# git, curl, and ca-certificates already ship in homebrew/brew:latest, so
# there's nothing to install for the HTTPS clone step. nvim is deliberately
# absent — the demo's first beat is `nvim .` failing with "command not found",
# and this base ships no command-not-found handler to muddy that message.

# cfgd enters as a prebuilt host binary, never built in-container: building
# Rust here would add a toolchain and minutes of compile time the demo has no
# use for. Staged at demo/bin/cfgd by `task build` before `docker build` runs
# (see .dockerignore — target/ itself never enters the build context).
COPY --chmod=0755 demo/bin/cfgd /usr/local/bin/cfgd

WORKDIR /home/linuxbrew/cfgd

# Real source tree for `nvim .` to open on both sides of the demo — this
# repo, filtered by the existing .dockerignore (crates/, examples/, tests/,
# Cargo.*, README.md). Never target/, which is gigabytes of build output the
# demo has no use for.
COPY --chown=linuxbrew:linuxbrew . .

USER linuxbrew
