#!/usr/bin/env bash
set -euo pipefail

REPO="endo-ava/ego-graph"
BIN_NAME="egopulse"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
SKIP_RUN="${EGOPULSE_INSTALL_SKIP_RUN:-0}"
RUN_SETUP="${EGOPULSE_INSTALL_RUN_SETUP:-0}"

log() {
  printf '%s\n' "$*"
}

err() {
  printf 'Error: %s\n' "$*" >&2
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

print_help() {
  cat <<'EOF'
Usage: install-egopulse.sh [--skip-run] [--setup]

Options:
  --skip-run   Do not auto-run egopulse after install.
  --setup      Run egopulse setup wizard after install.

Environment variables:
  EGOPULSE_INSTALL_SKIP_RUN   Set to "1" as an alternative to --skip-run.
  EGOPULSE_INSTALL_RUN_SETUP  Set to "1" as an alternative to --setup.
EOF
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --skip-run)
        SKIP_RUN=1
        ;;
      --setup)
        RUN_SETUP=1
        ;;
      -h|--help)
        print_help
        exit 0
        ;;
      *)
        err "Unknown argument: $1"
        print_help >&2
        exit 1
        ;;
    esac
    shift
  done
}

should_skip_run() {
  local skip_run_normalized
  skip_run_normalized="$(printf '%s' "$SKIP_RUN" | tr '[:upper:]' '[:lower:]')"
  case "$skip_run_normalized" in
    1|true|yes)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

detect_os() {
  case "$(uname -s)" in
    Darwin) echo "darwin" ;;
    Linux) echo "linux" ;;
    *)
      err "Unsupported OS: $(uname -s)"
      exit 1
      ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *)
      err "Unsupported architecture: $(uname -m)"
      exit 1
      ;;
  esac
}

detect_install_dir() {
  if [ -n "${EGOPULSE_INSTALL_DIR:-}" ]; then
    echo "$EGOPULSE_INSTALL_DIR"
    return
  fi
  if [ -w "/usr/local/bin" ]; then
    echo "/usr/local/bin"
    return
  fi
  if [ -d "$HOME/.local/bin" ] || mkdir -p "$HOME/.local/bin" 2>/dev/null; then
    echo "$HOME/.local/bin"
    return
  fi
  echo "/usr/local/bin"
}

download_release_json() {
  if need_cmd curl; then
    curl -fsSL "$API_URL"
  elif need_cmd wget; then
    wget -qO- "$API_URL"
  else
    err "Neither curl nor wget is available"
    exit 1
  fi
}

extract_asset_url() {
  local release_json="$1"
  local os="$2"
  local arch="$3"
  local os_regex arch_regex

  case "$os" in
    darwin) os_regex="apple-darwin|darwin" ;;
    linux) os_regex="unknown-linux-gnu|linux" ;;
    *)
      err "Unsupported OS for release matching: $os"
      return 1
      ;;
  esac

  case "$arch" in
    x86_64) arch_regex="x86_64|amd64" ;;
    aarch64) arch_regex="aarch64|arm64" ;;
    *)
      err "Unsupported architecture for release matching: $arch"
      return 1
      ;;
  esac

  printf '%s\n' "$release_json" \
    | grep -Eo 'https://[^"]+' \
    | grep '/releases/download/' \
    | grep -E "/${BIN_NAME}-[0-9]+\.[0-9]+\.[0-9]+-.*\.tar\.gz$" \
    | grep -Ei "(${arch_regex}).*(${os_regex})|(${os_regex}).*(${arch_regex})" \
    | head -n1
}

download_file() {
  local url="$1"
  local output="$2"
  if need_cmd curl; then
    curl -fL "$url" -o "$output"
  else
    wget -O "$output" "$url"
  fi
}

install_binary() {
  local archive="$1"
  local install_dir="$2"
  local tmpdir="$3"

  tar -xzf "$archive" -C "$tmpdir"

  local bin_path
  bin_path="$(find "$tmpdir" -type f -name "$BIN_NAME" | head -n1)"
  if [ -z "$bin_path" ]; then
    err "Could not find '$BIN_NAME' in archive"
    return 1
  fi

  chmod +x "$bin_path"
  local tmp_target target_path
  target_path="$install_dir/$BIN_NAME"
  tmp_target="$install_dir/.${BIN_NAME}.tmp.$$"
  if [ -w "$install_dir" ]; then
    cp "$bin_path" "$tmp_target"
    chmod +x "$tmp_target"
    mv -f "$tmp_target" "$target_path"
  else
    if need_cmd sudo; then
      sudo cp "$bin_path" "$tmp_target"
      sudo chmod +x "$tmp_target"
      sudo mv -f "$tmp_target" "$target_path"
    else
      err "No write permission for $install_dir and sudo not available"
      return 1
    fi
  fi
}

main() {
  local os arch install_dir release_json asset_url tmpdir archive asset_filename had_existing_bin

  parse_args "$@"

  os="$(detect_os)"
  arch="$(detect_arch)"
  install_dir="$(detect_install_dir)"
  had_existing_bin=0
  if need_cmd "${BIN_NAME}"; then
    had_existing_bin=1
  fi

  log "Installing ${BIN_NAME} for ${os}/${arch}..."
  release_json="$(download_release_json)"
  asset_url="$(extract_asset_url "$release_json" "$os" "$arch" || true)"
  if [ -z "$asset_url" ]; then
    err "No prebuilt binary found for ${os}/${arch} in the latest GitHub release."
    err "Use a separate install method instead:"
    err "  Build from source: cd ego-graph && cargo build --release -p egopulse"
    err "  Then install it: sudo install -m 0755 target/release/egopulse /usr/local/bin/egopulse"
    err "  Releases: https://github.com/${REPO}/releases"
    exit 1
  fi

  tmpdir="$(mktemp -d)"
  trap 'if [ -n "${tmpdir:-}" ]; then rm -rf "$tmpdir"; fi' EXIT
  asset_filename="${asset_url##*/}"
  asset_filename="${asset_filename%%\?*}"
  if [ -z "$asset_filename" ] || [ "$asset_filename" = "$asset_url" ]; then
    asset_filename="${BIN_NAME}.tar.gz"
  fi
  archive="$tmpdir/$asset_filename"
  log "Downloading: $asset_url"
  download_file "$asset_url" "$archive"
  install_binary "$archive" "$install_dir" "$tmpdir"

  log ""
  log "Installed ${BIN_NAME}."
  if [ "$install_dir" = "$HOME/.local/bin" ]; then
    log "Make sure '$HOME/.local/bin' is in PATH."
    log "Example: export PATH=\"\$HOME/.local/bin:\$PATH\""
  fi
  log "${BIN_NAME}"
  if should_skip_run; then
    log "Skipping auto-run (--skip-run)."
  elif [ "$had_existing_bin" -eq 1 ]; then
    log "Skipping auto-run (upgrade detected)."
  elif need_cmd "${BIN_NAME}"; then
    log "Running: ${BIN_NAME} --version"
    "${BIN_NAME}" --version
  else
    log "Could not find '${BIN_NAME}' in PATH."
    log "Add this directory to PATH: ${install_dir}"
    if [ "$install_dir" = "$HOME/.local/bin" ]; then
      log "Example: export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
    log "Then run: ${install_dir}/${BIN_NAME}"
  fi

  log ""
  log "Next steps:"
  log "  1. Run '${BIN_NAME} setup' to create your configuration"
  log "  2. Run '${BIN_NAME} start' to launch channel adapters"
  log "  3. Or run '${BIN_NAME}' to open the TUI"

  run_setup_normalized="$(printf '%s' "$RUN_SETUP" | tr '[:upper:]' '[:lower:]')"
  case "$run_setup_normalized" in
    1|true|yes)
      if need_cmd "${BIN_NAME}"; then
        log ""
        log "Running: ${BIN_NAME} setup"
        "${BIN_NAME}" setup
      else
        err "Cannot run setup: '${BIN_NAME}' not found in PATH"
      fi
      ;;
  esac
}

main "$@"
