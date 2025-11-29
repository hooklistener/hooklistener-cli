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

    EXPECTED=$(grep "$ARCHIVE_NAME" "$CHECKSUMS" | awk '{print $1}')

    if [ -z "$EXPECTED" ]; then
        warn "Could not find checksum for $ARCHIVE_NAME, skipping verification"
        return 0
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
    else
        warn "No SHA256 tool found, skipping checksum verification"
        return 0
    fi

    if [ "$EXPECTED" != "$ACTUAL" ]; then
        error "Checksum verification failed!\nExpected: $EXPECTED\nActual: $ACTUAL"
    fi

    success "Checksum verified"
}

# Main installation function
main() {
    info "Installing Hooklistener CLI..."

    check_dependencies

    PLATFORM=$(detect_platform)
    info "Detected platform: $PLATFORM"

    VERSION=$(get_latest_version)
    info "Latest version: $VERSION"

    ARCHIVE_NAME="hooklistener-cli-${PLATFORM}.tar.gz"
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

    info "Extracting archive..."
    tar -xzf "$ARCHIVE_PATH" -C "$TMP_DIR"

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

    # Create install directory if it doesn't exist
    $NEED_SUDO mkdir -p "$INSTALL_DIR"

    # Install the binary with the shorter name
    $NEED_SUDO cp "${TMP_DIR}/hooklistener-cli" "${INSTALL_DIR}/${BINARY_NAME}"
    $NEED_SUDO chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    # Verify installation
    if [ -x "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        printf "\n"
        printf "${GREEN}${BOLD}Hooklistener CLI installed successfully!${NC}\n"
        printf "\n"
        printf "  ${BOLD}Version${NC}:  %s\n" "$VERSION"
        printf "  ${BOLD}Location${NC}: %s\n" "${INSTALL_DIR}/${BINARY_NAME}"
        printf "\n"
        printf "${BOLD}Get started:${NC}\n"
        printf "  hooklistener tui      # Launch the terminal UI\n"
        printf "  hooklistener login    # Authenticate with your account\n"
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
