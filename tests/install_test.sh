#!/bin/sh
# Offline tests for install.sh: platform selection, URL building and install behavior.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
script="${here}/../install.sh"
fails=0

ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1" >&2; fails=$((fails + 1)); }

BITBUCKET_INSTALL_NO_MAIN=1
export BITBUCKET_INSTALL_NO_MAIN
# shellcheck disable=SC1090
. "${script}"
unset BITBUCKET_INSTALL_NO_MAIN
set +e

check_asset() {
  _got="$(asset_for "$1" "$2" 2>/dev/null)"
  if [ "${_got}" = "$3" ]; then ok "$1 $2 -> $3"; else bad "$1 $2 expected $3 got '${_got}'"; fi
}

check_unsupported() {
  _err="$(asset_for "$1" "$2" 2>&1 >/dev/null)"
  _rc=$?
  case "${_err}" in
    *"$1"*"$2"*"https://github.com/ejklock/bitbucket-cli/releases"*)
      if [ "${_rc}" -ne 0 ]; then ok "$1 $2 rejected"; else bad "$1 $2 exited 0"; fi ;;
    *) bad "$1 $2 message was '${_err}'" ;;
  esac
}

check_asset Linux x86_64 bitbucket-linux-amd64.tar.gz
check_asset Linux aarch64 bitbucket-linux-arm64.tar.gz
check_asset Linux arm64 bitbucket-linux-arm64.tar.gz
check_asset Darwin arm64 bitbucket-darwin-arm64.tar.gz
check_asset Darwin x86_64 bitbucket-darwin-amd64.tar.gz
check_unsupported FreeBSD amd64
check_unsupported Linux i686

_url="$(download_url https://example.test/releases v1.2.1 bitbucket-linux-amd64.tar.gz)"
if [ "${_url}" = "https://example.test/releases/download/v1.2.1/bitbucket-linux-amd64.tar.gz" ]; then
  ok "pinned version url"; else bad "pinned url was ${_url}"; fi
_url="$(download_url https://example.test/releases "" bitbucket-linux-amd64.tar.gz)"
if [ "${_url}" = "https://example.test/releases/latest/download/bitbucket-linux-amd64.tar.gz" ]; then
  ok "latest url"; else bad "latest url was ${_url}"; fi

# Install behavior against a file:// release tree.
work="$(mktemp -d "${TMPDIR:-/tmp}/install-test.XXXXXX")"
trap 'rm -rf "${work}"' EXIT INT TERM

if ! _host_asset="$(asset_for "$(uname -s)" "$(uname -m)" 2>/dev/null)"; then
  echo "host platform has no release asset; install cases skipped"
  [ "${fails}" -eq 0 ]; exit $?
fi

stage="${work}/stage"
mkdir -p "${stage}" "${work}/rel/latest/download" "${work}/rel/download/v9.9.9"
printf '#!/bin/sh\necho "bitbucket 9.9.9"\n' > "${stage}/bitbucket"
chmod 755 "${stage}/bitbucket"
tar -czf "${work}/rel/latest/download/${_host_asset}" -C "${stage}" bitbucket

run_install() { # dir [args...]
  _dir="$1"; shift
  ( BITBUCKET_INSTALL_DIR="${_dir}" BITBUCKET_INSTALL_BASE_URL="file://${work}/rel" \
    sh "${script}" "$@" ) >"${work}/out" 2>&1
}

mode_of() { if [ -n "$(find "$1" -type f -perm 755)" ]; then echo -rwxr-xr-x; else echo other; fi; }

dir="${work}/bin"
run_install "${dir}"; rc=$?
if [ "${rc}" -eq 0 ] && [ -x "${dir}/bitbucket" ] && [ "$(mode_of "${dir}/bitbucket")" = "-rwxr-xr-x" ]; then
  ok "installs binary with mode 0755"; else bad "install rc=${rc}: $(cat "${work}/out")"; fi
if [ "$("${dir}/bitbucket")" = "bitbucket 9.9.9" ]; then ok "installed binary runs"; else bad "binary does not run"; fi

dir="${work}/with space/bin dir"
run_install "${dir}"; rc=$?
if [ "${rc}" -eq 0 ] && [ -x "${dir}/bitbucket" ]; then ok "install dir with spaces"; else bad "spaces rc=${rc}: $(cat "${work}/out")"; fi

dir="${work}/missing"
run_install "${dir}" v0.0.0; rc=$?
if [ "${rc}" -ne 0 ] && [ ! -e "${dir}/bitbucket" ]; then ok "failed download leaves no binary"; else bad "download failure rc=${rc}"; fi

dir="${work}/corrupt"
mkdir -p "${dir}" "${work}/bad/latest/download"
printf 'old' > "${dir}/bitbucket"
printf 'not an archive' > "${work}/bad/latest/download/${_host_asset}"
( BITBUCKET_INSTALL_DIR="${dir}" BITBUCKET_INSTALL_BASE_URL="file://${work}/bad" sh "${script}" ) >"${work}/out" 2>&1; rc=$?
if [ "${rc}" -ne 0 ] && [ "$(cat "${dir}/bitbucket")" = "old" ]; then
  ok "corrupt archive keeps the existing binary"; else bad "corrupt rc=${rc}"; fi
leftover="$(find "${dir}" -type f ! -name bitbucket | wc -l | tr -d ' ')"
if [ "${leftover}" = "0" ]; then ok "corrupt archive leaves no partial file"; else bad "leftover files in ${dir}"; fi

dir="${work}/replace"
mkdir -p "${dir}"
printf 'old' > "${dir}/bitbucket"
run_install "${dir}"; rc=$?
if [ "${rc}" -eq 0 ] && [ "$("${dir}/bitbucket")" = "bitbucket 9.9.9" ]; then
  ok "existing binary replaced"; else bad "replace rc=${rc}"; fi

[ "${fails}" -eq 0 ]
