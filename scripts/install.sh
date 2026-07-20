#!/bin/sh
# Hooklistener CLI installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.sh | sh
set -eu

REPO="hooklistener/hooklistener-cli"
BINARY_NAME="hooklistener"
INSTALL_DIR="${HOOKLISTENER_INSTALL_DIR:-/usr/local/bin}"

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

info() {
    printf "${BLUE}info${NC}: %s\n" "$1"
}

success() {
    printf "${GREEN}success${NC}: %s\n" "$1"
}

warn() {
    printf "${YELLOW}warning${NC}: %s\n" "$1"
}

error() {
    printf "${RED}error${NC}: %s\n" "$1" >&2
    exit 1
}

# Detect the platform and return the target triple
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin)
            case "$ARCH" in
                arm64|aarch64)
                    echo "aarch64-apple-darwin"
                    ;;
                x86_64)
                    echo "x86_64-apple-darwin"
                    ;;
                *)
                    error "Unsupported architecture: $ARCH on macOS"
                    ;;
            esac
            ;;
        Linux)
            case "$ARCH" in
                x86_64)
                    echo "x86_64-unknown-linux-gnu"
                    ;;
                aarch64)
                    error "Linux ARM64 is not yet supported. Please build from source."
                    ;;
                *)
                    error "Unsupported architecture: $ARCH on Linux"
                    ;;
            esac
            ;;
        *)
            error "Unsupported operating system: $OS"
            ;;
    esac
}

# Check for required commands
check_dependencies() {
    if command -v curl >/dev/null 2>&1; then
        DOWNLOADER="curl"
    elif command -v wget >/dev/null 2>&1; then
        DOWNLOADER="wget"
    else
        error "Either curl or wget is required to download files"
    fi

    if ! command -v tar >/dev/null 2>&1; then
        error "tar is required to extract the archive"
    fi
}

# Download a file using curl or wget
download() {
    URL="$1"
    OUTPUT="$2"

    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL "$URL" -o "$OUTPUT"
    else
        wget -q "$URL" -O "$OUTPUT"
    fi
}

# Get the latest release version from GitHub
get_latest_version() {
    URL="https://api.github.com/repos/${REPO}/releases/latest"

    if [ "$DOWNLOADER" = "curl" ]; then
        VERSION=$(curl -fsSL "$URL" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
    else
        VERSION=$(wget -qO- "$URL" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
    fi

    if [ -z "$VERSION" ]; then
        error "Failed to get latest version from GitHub"
    fi

    echo "$VERSION"
}

# Verify SHA256 checksum
verify_checksum() {
    ARCHIVE="$1"
    CHECKSUMS="$2"
    ARCHIVE_NAME="$3"

    EXPECTED=$(awk -v archive_name="$ARCHIVE_NAME" '
        {
            listed_name = $2
            sub(/^\*/, "", listed_name)
            if (listed_name == archive_name) {
                matches++
                checksum = $1
                if (NF != 2) {
                    malformed = 1
                }
            }
        }
        END {
            if (matches != 1 || malformed) {
                exit 1
            }
            print checksum
        }
    ' "$CHECKSUMS") || error "Checksum manifest must contain exactly one entry for $ARCHIVE_NAME"

    if [ "${#EXPECTED}" -ne 64 ]; then
        error "Checksum manifest contains an invalid SHA256 digest for $ARCHIVE_NAME"
    fi
    case "$EXPECTED" in
        *[!0-9a-fA-F]*)
            error "Checksum manifest contains an invalid SHA256 digest for $ARCHIVE_NAME"
            ;;
    esac
    EXPECTED=$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
    else
        error "A SHA256 checksum tool (sha256sum or shasum) is required"
    fi
    ACTUAL=$(printf '%s' "$ACTUAL" | tr '[:upper:]' '[:lower:]')

    if [ "$EXPECTED" != "$ACTUAL" ]; then
        error "Checksum verification failed!\nExpected: $EXPECTED\nActual: $ACTUAL"
    fi

    VERIFIED_ARCHIVE_SHA256="$EXPECTED"
    success "Checksum verified"
}

# When privilege elevation is required, pass the archive through an already-open
# descriptor and verify those exact bytes again in a root-owned temporary
# directory. This prevents an unprivileged process from swapping the extracted
# binary between checksum verification and the privileged install.
install_verified_archive_as_root() {
    ARCHIVE="$1"
    DESTINATION="$2"
    EXPECTED="$3"

    sudo sh -c '
        set -eu
        umask 077
        destination=$1
        expected=$2
        root_tmp=$(mktemp -d /tmp/hooklistener-install.XXXXXX)
        trap '\''rm -rf "$root_tmp"'\'' EXIT HUP INT TERM

        archive="$root_tmp/release.tar.gz"
        binary="$root_tmp/hooklistener"
        cat <&3 > "$archive"

        if command -v sha256sum >/dev/null 2>&1; then
            actual=$(sha256sum "$archive" | awk '\''{print $1}'\'')
        elif command -v shasum >/dev/null 2>&1; then
            actual=$(shasum -a 256 "$archive" | awk '\''{print $1}'\'')
        else
            echo "error: a SHA256 checksum tool is required after privilege elevation" >&2
            exit 1
        fi

        if [ "$actual" != "$expected" ]; then
            echo "error: release archive changed after checksum verification" >&2
            exit 1
        fi

        member_count=$(tar -tzf "$archive" | awk '\''$0 == "hooklistener" { count++ } END { print count + 0 }'\'')
        if [ "$member_count" -ne 1 ]; then
            echo "error: release archive must contain exactly one root-level hooklistener binary" >&2
            exit 1
        fi

        tar -xOzf "$archive" hooklistener > "$binary"
        chmod 0755 "$binary"
        mkdir -p "$(dirname "$destination")"
        mv -f "$binary" "$destination"
    ' sh "$DESTINATION" "$EXPECTED" 3< "$ARCHIVE"
}

# Main installation function
main() {
    info "Installing Hooklistener CLI..."

    check_dependencies

    PLATFORM=$(detect_platform)
    info "Detected platform: $PLATFORM"

    VERSION=$(get_latest_version)
    info "Latest version: $VERSION"

    ARCHIVE_NAME="hooklistener-${PLATFORM}.tar.gz"
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE_NAME}"
    CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS.txt"

    # Create temp directory
    TMP_DIR=$(mktemp -d)
    trap 'rm -rf "$TMP_DIR"' EXIT

    ARCHIVE_PATH="${TMP_DIR}/${ARCHIVE_NAME}"
    CHECKSUMS_PATH="${TMP_DIR}/SHA256SUMS.txt"

    info "Downloading ${ARCHIVE_NAME}..."
    download "$DOWNLOAD_URL" "$ARCHIVE_PATH"

    info "Downloading checksums..."
    download "$CHECKSUMS_URL" "$CHECKSUMS_PATH"

    info "Verifying checksum..."
    verify_checksum "$ARCHIVE_PATH" "$CHECKSUMS_PATH" "$ARCHIVE_NAME"

    # Check if we need sudo
    NEED_SUDO=""
    if [ ! -w "$INSTALL_DIR" ]; then
        if command -v sudo >/dev/null 2>&1; then
            NEED_SUDO="sudo"
            info "Installing to $INSTALL_DIR (requires sudo)..."
        else
            error "Cannot write to $INSTALL_DIR and sudo is not available.\nSet HOOKLISTENER_INSTALL_DIR to a writable directory."
        fi
    else
        info "Installing to $INSTALL_DIR..."
    fi

    if [ -n "$NEED_SUDO" ]; then
        info "Re-verifying and extracting archive in a privileged temporary directory..."
        install_verified_archive_as_root \
            "$ARCHIVE_PATH" \
            "${INSTALL_DIR}/${BINARY_NAME}" \
            "$VERIFIED_ARCHIVE_SHA256"
    else
        info "Extracting archive..."
        tar -xzf "$ARCHIVE_PATH" -C "$TMP_DIR"
        mkdir -p "$INSTALL_DIR"
        cp "${TMP_DIR}/hooklistener" "${INSTALL_DIR}/${BINARY_NAME}"
        chmod 0755 "${INSTALL_DIR}/${BINARY_NAME}"
    fi

    # Verify installation
    if [ -x "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        printf "\n"
        printf "${GREEN}${BOLD}Hooklistener CLI installed successfully!${NC}\n"
        printf "\n"
        printf "  ${BOLD}Version${NC}:  %s\n" "$VERSION"
        printf "  ${BOLD}Location${NC}: %s\n" "${INSTALL_DIR}/${BINARY_NAME}"
        printf "\n"
        printf "${BOLD}Get started:${NC}\n"
        printf "  hooklistener login    # Authenticate with your account\n"
        printf "  hooklistener listen <endpoint-slug>  # Forward and inspect endpoint webhooks\n"
        printf "  hooklistener tunnel   # Expose your local server to the internet\n"
        printf "  hooklistener --help   # View all commands\n"
        printf "\n"

        # Check if install dir is in PATH
        case ":$PATH:" in
            *":$INSTALL_DIR:"*)
                ;;
            *)
                printf "${YELLOW}Note${NC}: %s is not in your PATH.\n" "$INSTALL_DIR"
                printf "Add it to your shell profile:\n"
                printf "  export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR"
                printf "\n"
                ;;
        esac
    else
        error "Installation failed - binary not found at ${INSTALL_DIR}/${BINARY_NAME}"
    fi
}

main "$@"
