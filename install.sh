#!/bin/sh
# Installs the bitbucket CLI from the GitHub releases of ejklock/bitbucket-cli.
#
#   curl -fsSL https://raw.githubusercontent.com/ejklock/bitbucket-cli/HEAD/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/ejklock/bitbucket-cli/HEAD/install.sh | sh -s -- v1.2.1
#
# Environment: BITBUCKET_VERSION (release tag), BITBUCKET_INSTALL_DIR (default
# ~/.local/bin), BITBUCKET_INSTALL_BASE_URL (release URL prefix, for mirrors and tests).
set -eu

RELEASES_URL="https://github.com/ejklock/bitbucket-cli/releases"
BIN_NAME="bitbucket"

asset_for() {
  _os="$1"
  _arch="$2"
  case "${_os}" in
    Linux) _platform="linux" ;;
    Darwin) _platform="darwin" ;;
    *) _platform="" ;;
  esac
  case "${_arch}" in
    x86_64|amd64) _cpu="amd64" ;;
    arm64|aarch64) _cpu="arm64" ;;
    *) _cpu="" ;;
  esac
  if [ -z "${_platform}" ] || [ -z "${_cpu}" ]; then
    echo "Unsupported platform: ${_os} ${_arch}" >&2
    echo "Supported: Linux and macOS on amd64 or arm64. Other builds: ${RELEASES_URL}" >&2
    return 1
  fi
  echo "${BIN_NAME}-${_platform}-${_cpu}.tar.gz"
}

download_url() {
  if [ -n "$2" ]; then
    echo "$1/download/$2/$3"
  else
    echo "$1/latest/download/$3"
  fi
}

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    echo "Error: curl or wget is required." >&2
    return 1
  fi
}

main() {
  _asset="$(asset_for "$(uname -s)" "$(uname -m)")"
  _tag="${1:-${BITBUCKET_VERSION:-}}"
  _url="$(download_url "${BITBUCKET_INSTALL_BASE_URL:-${RELEASES_URL}}" "${_tag}" "${_asset}")"
  _dir="${BITBUCKET_INSTALL_DIR:-${HOME}/.local/bin}"

  _work="$(mktemp -d)"
  trap 'rm -rf "${_work}"' EXIT INT TERM

  echo "Downloading ${_url}"
  fetch "${_url}" "${_work}/${_asset}"
  mkdir "${_work}/extract"
  tar -xzf "${_work}/${_asset}" -C "${_work}/extract"
  if [ ! -f "${_work}/extract/${BIN_NAME}" ]; then
    echo "Error: the archive does not contain ${BIN_NAME}." >&2
    return 1
  fi

  mkdir -p "${_dir}"
  _staged="${_dir}/.${BIN_NAME}.new.$$"
  trap 'rm -rf "${_work}" "${_staged}"' EXIT INT TERM
  cp "${_work}/extract/${BIN_NAME}" "${_staged}"
  chmod 755 "${_staged}"
  mv -f "${_staged}" "${_dir}/${BIN_NAME}"
  echo "Installed to ${_dir}/${BIN_NAME}"

  case ":${PATH}:" in
    *":${_dir}:"*) ;;
    *) echo "Warning: ${_dir} is not on your PATH; add it to your shell profile." >&2 ;;
  esac
}

if [ "${BITBUCKET_INSTALL_NO_MAIN:-}" != "1" ]; then
  main "$@"
fi
