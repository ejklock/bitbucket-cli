#!/bin/sh
# The plugin and npm package copies of the skill must be byte-identical: each
# ships on its own, so neither can read the other at install time. Paths resolve
# from this script's location, not the caller's working directory. A copy that
# is a symlink, or sits under a symlinked directory that resolves outside the
# repository, fails: such a link would ship nothing.
set -eu

root="$(cd -P "$(dirname "$0")/.." && pwd -P)"
plugin_skill="plugins/bitbucket/skills/bitbucket/SKILL.md"
package_skill="packages/bitbucket-agent/skills/bitbucket/SKILL.md"

status=0

for rel in "${plugin_skill}" "${package_skill}"; do
  if [ -L "${root}/${rel}" ]; then
    echo "${rel} is a symlink; it must be a regular file" >&2
    status=1
  elif [ ! -f "${root}/${rel}" ]; then
    echo "${rel} is missing" >&2
    status=1
  else
    real_dir="$(cd -P "$(dirname "${root}/${rel}")" && pwd -P)"
    case "${real_dir}" in
      "${root}"/*) ;;
      *)
        echo "${rel} resolves outside the repository through a symlinked directory" >&2
        status=1
        ;;
    esac
  fi
done

if [ "${status}" -eq 0 ] && ! cmp -s "${root}/${plugin_skill}" "${root}/${package_skill}"; then
  echo "${package_skill} differs from ${plugin_skill}" >&2
  status=1
fi

if [ "${status}" -ne 0 ]; then
  exit 1
fi
echo "plugin and bitbucket-agent SKILL.md copies are identical"
