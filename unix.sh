#!/usr/bin/env bash
set -Eeuo pipefail

readonly REPOSITORY="Theryston/workstate"
readonly BINARY_NAME="workstate"
readonly COSMICMSG_URL="https://files.theryston.dev/cosmic/cosmicmsg"
readonly RELEASE_API_URL="https://api.github.com/repos/${REPOSITORY}/releases/latest"
readonly INSTALL_DIR="${XDG_BIN_HOME:-${HOME}/.local/bin}"

TMP_DIR=""
APT_UPDATED=0

if [ -n "${NO_COLOR:-}" ] || [ ! -t 1 ]; then
    RED=""
    GREEN=""
    YELLOW=""
    BLUE=""
    CYAN=""
    BOLD=""
    RESET=""
else
    RED=$'\033[31m'
    GREEN=$'\033[32m'
    YELLOW=$'\033[33m'
    BLUE=$'\033[34m'
    CYAN=$'\033[36m'
    BOLD=$'\033[1m'
    RESET=$'\033[0m'
fi

cleanup() {
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf -- "$TMP_DIR"
    fi
}

trap cleanup EXIT

abort() {
    printf '\n%sError:%s %s\n' "$RED" "$RESET" "$1" >&2
    exit 1
}

step() {
    printf '\n%s%s%s %s\n' "$BLUE" "$BOLD" "$1" "$RESET"
}

success() {
    printf '%s✓%s %s\n' "$GREEN" "$RESET" "$1"
}

notice() {
    printf '%s!%s %s\n' "$YELLOW" "$RESET" "$1"
}

read_os_release_value() {
    local key="$1"

    sed -n "s/^${key}=//p" /etc/os-release | head -n 1 | sed -e 's/^"//' -e 's/"$//'
}

detect_platform() {
    local kernel os_id os_name desktop_hint desktop_hint_lower

    kernel="$(uname -s)"
    if [ "$kernel" != "Linux" ] || [ ! -r /etc/os-release ]; then
        abort "This installer does not support this system yet. Supported platform: Pop!_OS with COSMIC on Linux."
    fi

    os_id="$(read_os_release_value ID)"
    os_name="$(read_os_release_value NAME)"
    desktop_hint="${XDG_CURRENT_DESKTOP:-} ${XDG_SESSION_DESKTOP:-} ${DESKTOP_SESSION:-}"
    desktop_hint_lower="$(printf '%s' "$desktop_hint" | tr '[:upper:]' '[:lower:]')"

    if [ "${os_id,,}" != "pop" ] || [[ "$desktop_hint_lower" != *cosmic* ]]; then
        printf '\n%sError:%s This installer does not support this system yet.\n' "$RED" "$RESET" >&2
        printf 'Detected operating system: %s\n' "${os_name:-${os_id:-unknown}}" >&2
        printf 'Detected desktop environment: %s\n' "${desktop_hint:-unknown}" >&2
        printf 'Currently supported: Pop!_OS with COSMIC on Linux.\n' >&2
        exit 1
    fi

    success "Supported platform detected: Pop!_OS with COSMIC on Linux"
}

detect_architecture() {
    case "$(uname -m)" in
        x86_64|amd64)
            printf '%s\n' "x86_64"
            ;;
        aarch64|arm64)
            printf '%s\n' "aarch64"
            ;;
        *)
            abort "Unsupported architecture. Workstate releases are available for x86_64 and aarch64 Linux systems."
            ;;
    esac
}

require_sudo() {
    if ! command -v sudo >/dev/null 2>&1; then
        abort "sudo is required to install system packages and cosmicmsg into /usr/local/bin."
    fi

    if ! sudo -v; then
        abort "Could not obtain sudo permissions."
    fi
}

refresh_apt() {
    if [ "$APT_UPDATED" -eq 1 ]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        abort "apt-get is required on Pop!_OS to install missing dependencies."
    fi

    require_sudo
    sudo apt-get update
    APT_UPDATED=1
}

ensure_download_tool() {
    if command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1; then
        return
    fi

    notice "Neither curl nor wget is installed. Installing curl."
    refresh_apt
    sudo apt-get install -y curl

    if ! command -v curl >/dev/null 2>&1; then
        abort "curl could not be installed, so release files cannot be downloaded."
    fi
}

download_file() {
    local url="$1"
    local destination="$2"

    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location --retry 3 --retry-delay 1 \
            --connect-timeout 15 --max-time 600 --output "$destination" "$url"
        return
    fi

    if command -v wget >/dev/null 2>&1; then
        wget --https-only --tries=3 --timeout=15 --output-document="$destination" "$url"
        return
    fi

    return 1
}

download_text() {
    local url="$1"

    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location --retry 3 --retry-delay 1 \
            --connect-timeout 15 --max-time 60 "$url"
        return
    fi

    if command -v wget >/dev/null 2>&1; then
        wget --https-only --tries=3 --timeout=15 --output-document=- "$url"
        return
    fi

    return 1
}

install_tmux() {
    step "Checking tmux"

    if command -v tmux >/dev/null 2>&1; then
        success "tmux is already installed"
        return
    fi

    notice "tmux is not installed. Installing it with apt."
    refresh_apt
    sudo apt-get install -y tmux

    if ! command -v tmux >/dev/null 2>&1; then
        abort "tmux installation completed without a usable tmux command."
    fi

    success "tmux installed successfully"
}

install_cosmicmsg() {
    step "Checking cosmicmsg"

    if command -v cosmicmsg >/dev/null 2>&1; then
        success "cosmicmsg is already installed"
        return
    fi

    ensure_download_tool
    local cosmicmsg_path="${TMP_DIR}/cosmicmsg"

    printf '%sDownloading cosmicmsg...%s\n' "$CYAN" "$RESET"
    if ! download_file "$COSMICMSG_URL" "$cosmicmsg_path"; then
        abort "Could not download cosmicmsg from ${COSMICMSG_URL}."
    fi

    if [ ! -s "$cosmicmsg_path" ]; then
        abort "The downloaded cosmicmsg file is empty."
    fi

    require_sudo
    if ! sudo install -m 0755 "$cosmicmsg_path" /usr/local/bin/cosmicmsg; then
        abort "Could not install cosmicmsg into /usr/local/bin."
    fi

    if [ ! -x /usr/local/bin/cosmicmsg ]; then
        abort "cosmicmsg was installed but is not executable at /usr/local/bin/cosmicmsg."
    fi

    success "cosmicmsg installed at /usr/local/bin/cosmicmsg"
}

install_workstate() {
    local architecture target archive_name archive_path checksum_path release_json latest_release download_url checksum_url

    step "Installing workstate"
    architecture="$(detect_architecture)"
    target="${architecture}-unknown-linux-gnu"
    archive_name="${BINARY_NAME}-${target}.tar.gz"
    archive_path="${TMP_DIR}/${archive_name}"
    checksum_path="${TMP_DIR}/checksums-sha256.txt"

    ensure_download_tool
    printf '%sFetching the latest release...%s\n' "$CYAN" "$RESET"
    if ! release_json="$(download_text "$RELEASE_API_URL")"; then
        abort "Could not fetch the latest workstate release from GitHub."
    fi

    latest_release="$(printf '%s' "$release_json" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
    if [ -z "$latest_release" ]; then
        abort "GitHub did not return a latest workstate release."
    fi

    download_url="https://github.com/${REPOSITORY}/releases/download/${latest_release}/${archive_name}"
    checksum_url="https://github.com/${REPOSITORY}/releases/download/${latest_release}/checksums-sha256.txt"

    printf '%sDownloading %s for %s...%s\n' "$CYAN" "$BINARY_NAME" "$target" "$RESET"
    if ! download_file "$download_url" "$archive_path"; then
        abort "Could not download ${archive_name} from release ${latest_release}."
    fi

    printf '%sVerifying release checksum...%s\n' "$CYAN" "$RESET"
    if ! download_file "$checksum_url" "$checksum_path"; then
        abort "Could not download the checksum file for release ${latest_release}."
    fi

    if ! command -v sha256sum >/dev/null 2>&1; then
        abort "sha256sum is required to verify the workstate release."
    fi

    if ! (cd "$TMP_DIR" && grep -F "  ${archive_name}" checksums-sha256.txt | sha256sum --check --status -); then
        abort "The downloaded workstate archive failed checksum verification."
    fi

    if ! tar -xzf "$archive_path" -C "$TMP_DIR"; then
        abort "Could not extract the workstate release archive."
    fi

    if [ ! -f "${TMP_DIR}/${BINARY_NAME}" ]; then
        abort "The release archive does not contain the ${BINARY_NAME} binary."
    fi

    mkdir -p "$INSTALL_DIR"
    if ! install -m 0755 "${TMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"; then
        abort "Could not install ${BINARY_NAME} into ${INSTALL_DIR}."
    fi

    success "workstate ${latest_release} installed at ${INSTALL_DIR}/${BINARY_NAME}"
}

path_contains() {
    case ":${PATH:-}:" in
        *":$1:"*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

persist_install_path() {
    local shell_name profile path_line

    if path_contains "$INSTALL_DIR"; then
        export PATH="${PATH:-}"
        return
    fi

    shell_name="$(basename "${SHELL:-sh}")"
    case "$shell_name" in
        zsh)
            profile="${HOME}/.zshrc"
            path_line="export PATH=\"\$PATH:${INSTALL_DIR}\""
            ;;
        bash)
            if [ -f "${HOME}/.bashrc" ]; then
                profile="${HOME}/.bashrc"
            else
                profile="${HOME}/.bash_profile"
            fi
            path_line="export PATH=\"\$PATH:${INSTALL_DIR}\""
            ;;
        fish)
            profile="${HOME}/.config/fish/config.fish"
            path_line="set -gx PATH \"${INSTALL_DIR}\" \$PATH"
            ;;
        *)
            profile="${HOME}/.profile"
            path_line="export PATH=\"\$PATH:${INSTALL_DIR}\""
            ;;
    esac

    mkdir -p "$(dirname "$profile")"
    if [ ! -f "$profile" ] || ! grep -Fq -- "$INSTALL_DIR" "$profile"; then
        printf '\n%s\n' "$path_line" >> "$profile"
        notice "Added ${INSTALL_DIR} to ${profile}"
    fi

    export PATH="${PATH:+${PATH}:}${INSTALL_DIR}"
}

main() {
    if [ "$(id -u)" -eq 0 ]; then
        abort "Run this installer as your normal desktop user, not as root."
    fi

    printf '%s%sWorkstate Installer%s\n' "$CYAN" "$BOLD" "$RESET"
    printf 'Install the latest Workstate release for Pop!_OS + COSMIC.\n'

    step "Checking platform compatibility"
    detect_platform

    TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/workstate-install.XXXXXX")"
    install_tmux
    install_cosmicmsg
    install_workstate
    persist_install_path

    if [ -x "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        success "Installation completed"
        printf '\nRun %sworkstate --help%s to get started.\n' "$YELLOW" "$RESET"
        if ! path_contains "$INSTALL_DIR"; then
            printf 'Open a new shell before running workstate, or export PATH manually:\n'
            printf '  export PATH="\$PATH:%s"\n' "$INSTALL_DIR"
        fi
    else
        abort "The workstate binary was not found after installation."
    fi
}

main "$@"
