#!/bin/sh
# Proves scripts/check-skill-copies.sh passes on identical copies and fails,
# naming the file, on each drift it guards. Every case runs on a fresh copy of
# the script and the two skill files under a mktemp dir; the repository is
# never edited.
set -u

repo="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT INT TERM

plugin_skill=plugins/bitbucket/skills/bitbucket/SKILL.md
package_skill=packages/bitbucket-agent/skills/bitbucket/SKILL.md

failures=0
out=""
code=0

fail() {
  echo "FAIL $1: $2" >&2
  failures=$((failures + 1))
}

# fresh_tree: copy the script and both skills into a new tree and print its path.
fresh_tree() {
  tree="$(mktemp -d "${work}/tree.XXXXXX")"
  mkdir -p "${tree}/scripts" "$(dirname "${tree}/${plugin_skill}")" "$(dirname "${tree}/${package_skill}")"
  cp "${repo}/scripts/check-skill-copies.sh" "${tree}/scripts/" 2>/dev/null
  cp "${repo}/${plugin_skill}" "${tree}/${plugin_skill}"
  cp "${repo}/${package_skill}" "${tree}/${package_skill}"
  echo "${tree}"
}

# run_check TREE CWD: run TREE's copy of the check from CWD.
run_check() {
  out="$(cd "$2" && sh "$1/scripts/check-skill-copies.sh" 2>&1)"
  code=$?
}

expect_green() {
  if [ "${code}" -ne 0 ]; then
    fail "$1" "exit ${code}, expected 0: ${out}"
  fi
}

# expect_red NAME WANT: last run must exit non-zero and its output hold WANT.
expect_red() {
  if [ "${code}" -eq 0 ]; then
    fail "$1" "exit 0, expected non-zero"
    return
  fi
  case "${out}" in
    *"$2"*) ;;
    *) fail "$1" "output lacks '$2': ${out}" ;;
  esac
}

test_identical_copies_pass() {
  t="$(fresh_tree)"
  run_check "${t}" "${t}"
  expect_green identical_copies_pass
}

test_one_byte_difference_fails_naming_the_file() {
  t="$(fresh_tree)"
  printf 'x' >>"${t}/${package_skill}"
  run_check "${t}" "${t}"
  expect_red one_byte_difference_fails_naming_the_file "${package_skill}"
}

test_missing_plugin_copy_fails_naming_the_file() {
  t="$(fresh_tree)"
  rm "${t}/${plugin_skill}"
  run_check "${t}" "${t}"
  expect_red missing_plugin_copy_fails_naming_the_file "${plugin_skill}"
}

test_missing_package_copy_fails_naming_the_file() {
  t="$(fresh_tree)"
  rm "${t}/${package_skill}"
  run_check "${t}" "${t}"
  expect_red missing_package_copy_fails_naming_the_file "${package_skill}"
}

test_other_working_directory_passes_identical_copies() {
  t="$(fresh_tree)"
  other="$(mktemp -d "${work}/other.XXXXXX")"
  run_check "${t}" "${other}"
  expect_green other_working_directory_passes_identical_copies
}

test_other_working_directory_reports_drift_not_missing() {
  t="$(fresh_tree)"
  printf 'x' >>"${t}/${package_skill}"
  other="$(mktemp -d "${work}/other.XXXXXX")"
  run_check "${t}" "${other}"
  expect_red other_working_directory_reports_drift_not_missing "differs from"
  case "${out}" in
    *"is missing"*) fail other_working_directory_reports_drift_not_missing "reported missing: ${out}" ;;
  esac
}

test_symlink_to_a_file_outside_the_repository_fails() {
  t="$(fresh_tree)"
  outside="$(mktemp -d "${work}/outside.XXXXXX")"
  cp "${t}/${plugin_skill}" "${outside}/SKILL.md"
  rm "${t}/${package_skill}"
  ln -s "${outside}/SKILL.md" "${t}/${package_skill}"
  run_check "${t}" "${t}"
  expect_red symlink_to_a_file_outside_the_repository_fails "${package_skill}"
}

test_symlinked_parent_directory_outside_the_repository_fails() {
  t="$(fresh_tree)"
  outside="$(mktemp -d "${work}/outside.XXXXXX")"
  mv "${t}/$(dirname "${package_skill}")" "${outside}/bitbucket"
  ln -s "${outside}/bitbucket" "${t}/$(dirname "${package_skill}")"
  run_check "${t}" "${t}"
  expect_red symlinked_parent_directory_outside_the_repository_fails "${package_skill}"
}

for t in \
  test_identical_copies_pass \
  test_one_byte_difference_fails_naming_the_file \
  test_missing_plugin_copy_fails_naming_the_file \
  test_missing_package_copy_fails_naming_the_file \
  test_other_working_directory_passes_identical_copies \
  test_other_working_directory_reports_drift_not_missing \
  test_symlink_to_a_file_outside_the_repository_fails \
  test_symlinked_parent_directory_outside_the_repository_fails; do
  "$t"
done

if [ "${failures}" -ne 0 ]; then
  echo "${failures} failure(s)" >&2
  exit 1
fi
echo "check-skill-copies: all cases pass"
