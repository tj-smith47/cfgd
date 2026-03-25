#!/bin/sh
# cfgd installer — detects OS/arch, downloads the correct binary, verifies checksum, installs to PATH
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh
#   curl -fsSL ... | sh -s -- init --from <url>
#
# Environment variables:
#   CFGD_INSTALL_DIR  — override install directory (default: /usr/local/bin or ~/.local/bin)
#   CFGD_VERSION      — override version to install (default: latest)
#   CFGD_REPO         — override GitHub repo (default: tj-smith47/cfgd)

set -eu

REPO="${CFGD_REPO:-tj-smith47/cfgd}"
VERSION="${CFGD_VERSION:-latest}"
INSTALL_DIR="${CFGD_INSTALL_DIR:-}"
DRY_RUN=false

# --- Helpers ---

info() {
    printf '\033[0;36m%s\033[0m %s\n' "●" "$*"
}

success() {
    printf '\033[0;32m%s\033[0m %s\n' "✓" "$*"
}

error() {
    printf '\033[0;31m%s\033[0m %s\n' "✗" "$*" >&2
}

warn() {
    printf '\033[0;33m%s\033[0m %s\n' "⚠" "$*"
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Download a URL to a file. Requires curl or wget (checked in main before calling).
fetch() {
    local url="$1" dest="$2"
    if command_exists curl; then
        curl -fsSL -o "$dest" "$url"
    else
        wget -q -O "$dest" "$url"
    fi
}

# Download a URL and print to stdout.
fetch_stdout() {
    local url="$1"
    if command_exists curl; then
        curl -fsSL "$url"
    else
        wget -qO- "$url"
    fi
}

usage() {
    cat <<EOF
cfgd installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh
  curl -fsSL ... | sh -s -- [OPTIONS] [-- CFGD_ARGS...]

Options:
  --help       Show this help message
  --dry-run    Print what would be done without making changes
  --version V  Install a specific version (default: latest)

Environment variables:
  CFGD_INSTALL_DIR  Override install directory (default: /usr/local/bin or ~/.local/bin)
  CFGD_VERSION      Override version to install (default: latest)
  CFGD_REPO         Override GitHub repo (default: tj-smith47/cfgd)

Alternative install methods:
  brew install tj-smith47/tap/cfgd
  cargo install cfgd
EOF
}

# --- OS / Architecture Detection ---

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        *)
            error "Unsupported OS: $(uname -s)"
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        arm64|aarch64) echo "aarch64" ;;
        *)
            error "Unsupported architecture: $(uname -m)"
            exit 1
            ;;
    esac
}

# --- Install Directory ---

resolve_install_dir() {
    if [ -n "$INSTALL_DIR" ]; then
        echo "$INSTALL_DIR"
        return
    fi

    # Prefer /usr/local/bin if writable, otherwise ~/.local/bin
    if [ -w /usr/local/bin ]; then
        echo "/usr/local/bin"
    else
        local_bin="${HOME}/.local/bin"
        mkdir -p "$local_bin"
        echo "$local_bin"
    fi
}

# --- Download ---

resolve_version() {
    if [ "$VERSION" = "latest" ]; then
        VERSION=$(fetch_stdout "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')

        if [ -z "$VERSION" ]; then
            error "Could not determine latest version from GitHub"
            exit 1
        fi
    fi

    # Strip leading 'v' for download URL construction
    VERSION_NUM="${VERSION#v}"
}

download_and_install() {
    local os="$1"
    local arch="$2"
    local dest_dir="$3"

    local archive_name="cfgd-${VERSION_NUM}-${os}-${arch}.tar.gz"
    local checksum_name="cfgd-${VERSION_NUM}-checksums.txt"
    local base_url="https://github.com/${REPO}/releases/download/${VERSION}"

    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] Would download ${base_url}/${archive_name}"
        info "[dry-run] Would verify checksum from ${base_url}/${checksum_name}"
        info "[dry-run] Would extract and install cfgd to ${dest_dir}/cfgd"
        if [ ! -w "$dest_dir" ]; then
            info "[dry-run] Would require sudo for ${dest_dir}"
        fi
        return
    fi

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    info "Downloading cfgd ${VERSION} for ${os}/${arch}..."

    fetch "${base_url}/${archive_name}" "${tmp_dir}/${archive_name}" || {
        error "Download failed: ${base_url}/${archive_name}"
        exit 1
    }

    # Download and verify checksum
    if fetch "${base_url}/${checksum_name}" "${tmp_dir}/${checksum_name}" 2>/dev/null; then
        info "Verifying checksum..."

        local sha_cmd=""
        if [ "$(uname -s)" = "Darwin" ] && command_exists shasum; then
            # Prefer native shasum on macOS — Homebrew sha256sum can behave differently
            sha_cmd="shasum -a 256"
        elif command_exists sha256sum; then
            sha_cmd="sha256sum"
        elif command_exists shasum; then
            sha_cmd="shasum -a 256"
        fi

        if [ -n "$sha_cmd" ]; then
            cd "$tmp_dir"
            grep "  ${archive_name}$" "$checksum_name" | $sha_cmd -c >/dev/null 2>&1 || {
                error "Checksum verification failed"
                exit 1
            }
            cd - >/dev/null
            success "Checksum verified"
        else
            warn "No sha256sum/shasum found — skipping checksum verification"
        fi
    else
        warn "Checksums not available — skipping verification"
    fi

    # Extract
    info "Extracting..."
    tar -xzf "${tmp_dir}/${archive_name}" -C "$tmp_dir"

    # Install binary
    if [ -w "$dest_dir" ]; then
        cp "${tmp_dir}/cfgd" "${dest_dir}/cfgd"
        chmod +x "${dest_dir}/cfgd"
    else
        info "Installing to ${dest_dir} (requires sudo)..."
        sudo cp "${tmp_dir}/cfgd" "${dest_dir}/cfgd"
        sudo chmod +x "${dest_dir}/cfgd"
    fi

    success "Installed cfgd ${VERSION} to ${dest_dir}/cfgd"
}

# --- PATH check ---

ensure_in_path() {
    local dir="$1"
    case ":${PATH}:" in
        *":${dir}:"*) return 0 ;;
    esac

    warn "${dir} is not in your PATH"
    info "Add it by appending to your shell profile:"
    info "  export PATH=\"${dir}:\$PATH\""
}

# --- Main ---

main() {
    # Parse installer flags (before forwarding remaining args to cfgd)
    local cfgd_args=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --help)
                usage
                exit 0
                ;;
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            --version)
                VERSION="$2"
                shift 2
                ;;
            *)
                # Remaining args are forwarded to cfgd
                cfgd_args="$*"
                break
                ;;
        esac
    done

    info "cfgd installer"
    echo ""

    # Check prerequisites
    if ! command_exists curl && ! command_exists wget; then
        if command_exists brew; then
            error "Neither curl nor wget found"
            info "Homebrew is available — install with:"
            info "  brew install tj-smith47/tap/cfgd"
        elif command_exists cargo; then
            error "Neither curl nor wget found"
            info "Cargo is available — install with:"
            info "  cargo install cfgd"
        else
            error "Neither curl nor wget found — install one to continue"
            info "Alternative install methods:"
            info "  brew install tj-smith47/tap/cfgd"
            info "  cargo install cfgd"
        fi
        exit 1
    fi

    local os arch dest_dir
    os="$(detect_os)"
    arch="$(detect_arch)"
    dest_dir="$(resolve_install_dir)"

    info "Detected: ${os}/${arch}"
    info "Install directory: ${dest_dir}"

    # Check for existing installation
    if command_exists cfgd; then
        local current
        current="$(cfgd --version 2>/dev/null || echo "unknown")"
        info "Existing installation: ${current}"
    fi

    resolve_version
    info "Version: ${VERSION}"
    echo ""

    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] No changes will be made"
        echo ""
    fi

    download_and_install "$os" "$arch" "$dest_dir"
    ensure_in_path "$dest_dir"

    echo ""
    success "Installation complete!"
    echo ""

    if [ "$DRY_RUN" = true ]; then
        return
    fi

    # If arguments were passed, forward them to cfgd
    if [ -n "$cfgd_args" ]; then
        info "Running: cfgd ${cfgd_args}"
        echo ""
        # Re-split cfgd_args back into positional params
        eval "set -- $cfgd_args"
        exec "${dest_dir}/cfgd" "$@"
    else
        info "Get started:"
        info "  cfgd init --from <repo-url>    # bootstrap from a git repo"
        info "  cfgd doctor                    # check system health"
        info "  cfgd --help                    # see all commands"
    fi
}

main "$@"
