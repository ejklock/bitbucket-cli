#!/bin/sh
# Offline tests: the release PKGBUILD and APKBUILD build this fork at the crate's version.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="${here}/.."
fails=0
fork="github.com/ejklock/bitbucket-cli"

ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1" >&2; fails=$((fails + 1)); }

crate_version() {
  sed -n 's/^version *= *"\(.*\)"/\1/p' "$1/Cargo.toml" | head -n 1
}

# field FILE NAME: the value of a top-level NAME=value line, quotes stripped.
field() {
  sed -n "s/^$2=[\"(]*\\([^\"]*\\).*/\\1/p" "$1" | head -n 1
}

# check_file ROOT RELPATH: prints one problem per line; empty output means it passes.
check_file() {
  _f="$1/$2"
  _want="$(crate_version "$1")"
  _ver="$(field "${_f}" pkgver)"
  [ "${_ver}" = "${_want}" ] || echo "$2: pkgver '${_ver}' is not crate version '${_want}'"
  case "$(field "${_f}" url)" in
    "https://${fork}") ;;
    *) echo "$2: url is not exactly https://${fork}" ;;
  esac
  _src="$(grep '^source=' "${_f}" || true)"
  case "${_src}" in
    *"::https://${fork}/archive/"* | *'"https://'"${fork}/archive/"* | "source=https://${fork}/archive/"*) ;;
    *) echo "$2: source does not start at https://${fork}/archive/" ;;
  esac
  case "${_src}" in
    *quinnjr*) echo "$2: source names quinnjr" ;;
  esac
}

expect_pass() {
  _out="$(check_file "$1" "$2")"
  if [ -z "${_out}" ]; then ok "$3"; else bad "$3: ${_out}"; fi
}

expect_fail() {
  _out="$(check_file "$1" "$2")"
  case "${_out}" in
    "$2"*) ok "$3" ;;
    *) bad "$3: expected a problem naming $2, got '${_out}'" ;;
  esac
}

scratch="$(mktemp -d "${TMPDIR:-/tmp}/packaging-test.XXXXXX")"
trap 'rm -rf "${scratch}"' EXIT
mkdir -p "${scratch}/packaging/arch" "${scratch}/packaging/alpine"
cp "${root}/Cargo.toml" "${scratch}/Cargo.toml"

arch=packaging/arch/PKGBUILD
alpine=packaging/alpine/APKBUILD

expect_pass "${root}" "${arch}" "PKGBUILD builds this fork at the crate version"
expect_pass "${root}" "${alpine}" "APKBUILD builds this fork at the crate version"

for f in "${arch}" "${alpine}"; do
  sed 's/^pkgver=.*/pkgver=1.0.1/' "${root}/${f}" > "${scratch}/${f}"
  expect_fail "${scratch}" "${f}" "${f} with a stale pkgver fails"
  sed '/^source=/s#ejklock#quinnjr#g' "${root}/${f}" > "${scratch}/${f}"
  expect_fail "${scratch}" "${f}" "${f} with an upstream source fails"
  sed 's#^url="\(.*\)bitbucket-cli"#url="\1bitbucket-cli-other"#' "${root}/${f}" > "${scratch}/${f}"
  expect_fail "${scratch}" "${f}" "${f} with a similarly named url fails"
  sed '/^source=/s#bitbucket-cli/archive#bitbucket-cli-other/archive#' "${root}/${f}" > "${scratch}/${f}"
  expect_fail "${scratch}" "${f}" "${f} with a similarly named source fails"
done

[ "${fails}" -eq 0 ]
