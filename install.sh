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
        if command_exists curl; then
            VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
                | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
        elif command_exists wget; then
            VERSION=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
                | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
        else
            error "Neither curl nor wget found — install one to continue"
            exit 1
        fi

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

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    info "Downloading cfgd ${VERSION} for ${os}/${arch}..."

    # Download archive
    if command_exists curl; then
        curl -fsSL -o "${tmp_dir}/${archive_name}" "${base_url}/${archive_name}" || {
            error "Download failed: ${base_url}/${archive_name}"
            exit 1
        }
    else
        wget -q -O "${tmp_dir}/${archive_name}" "${base_url}/${archive_name}" || {
            error "Download failed: ${base_url}/${archive_name}"
            exit 1
        }
    fi

    # Download checksums
    local checksum_verified=false
    if command_exists curl; then
        curl -fsSL -o "${tmp_dir}/${checksum_name}" "${base_url}/${checksum_name}" 2>/dev/null && checksum_verified=true
    elif command_exists wget; then
        wget -q -O "${tmp_dir}/${checksum_name}" "${base_url}/${checksum_name}" 2>/dev/null && checksum_verified=true
    fi

    # Verify checksum
    if [ "$checksum_verified" = true ]; then
        info "Verifying checksum..."
        cd "$tmp_dir"
        if command_exists sha256sum; then
            grep "$archive_name" "$checksum_name" | sha256sum -c --quiet 2>/dev/null || {
                error "Checksum verification failed"
                exit 1
            }
            success "Checksum verified"
        elif command_exists shasum; then
            grep "$archive_name" "$checksum_name" | shasum -a 256 -c --quiet 2>/dev/null || {
                error "Checksum verification failed"
                exit 1
            }
            success "Checksum verified"
        else
            warn "No sha256sum/shasum found — skipping checksum verification"
        fi
        cd - >/dev/null
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
    info "cfgd installer"
    echo ""

    # Check prerequisites
    if ! command_exists curl && ! command_exists wget; then
        error "Neither curl nor wget found — install one to continue"
        exit 1
    fi

    local os arch dest_dir
    os="$(detect_os)"
    arch="$(detect_arch)"
    dest_dir="$(resolve_install_dir)"

    info "Detected: ${os}/${arch}"
    info "Install directory: ${dest_dir}"

    resolve_version
    info "Version: ${VERSION}"
    echo ""

    download_and_install "$os" "$arch" "$dest_dir"
    ensure_in_path "$dest_dir"

    echo ""
    success "Installation complete!"
    echo ""

    # If arguments were passed, forward them to cfgd
    if [ $# -gt 0 ]; then
        info "Running: cfgd $*"
        echo ""
        exec "${dest_dir}/cfgd" "$@"
    else
        info "Get started:"
        info "  cfgd init --from <repo-url>    # bootstrap from a git repo"
        info "  cfgd doctor                    # check system health"
        info "  cfgd --help                    # see all commands"
    fi
}

main "$@"
