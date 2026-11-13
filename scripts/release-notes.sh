#!/usr/bin/env bash
# The GitHub release title and text of a version, taken from its section in CHANGELOG.md
# (.github/workflows/release.yml; docs/releasing.md, "Publishing a release").
#
#   scripts/release-notes.sh title <version> [changelog]   "Gono Forum " and the section heading without "## "
#   scripts/release-notes.sh notes <version> [changelog]   the section without its heading
#
# A section starts at a line "## <version>", optionally followed by a space and a label such as "(early alpha)", and
# ends before the next line that starts with "## ". The script fails when CHANGELOG.md has no section for the version,
# more than one, or one without text, so that a release is never published without notes.
set -euo pipefail

usage() {
  echo "usage: $0 title|notes <version> [changelog]" >&2
  exit 2
}

[ $# -ge 2 ] && [ $# -le 3 ] || usage
mode=$1
version=${2#v}
changelog=${3:-"$(dirname "$0")/../CHANGELOG.md"}
case "$mode" in title | notes) ;; *) usage ;; esac
[ -n "$version" ] || usage
[ -f "$changelog" ] || {
  echo "$changelog not found" >&2
  exit 1
}

# Exact field comparison rather than a regular expression, so the dots in the version match only dots.
count=$(awk -v v="$version" '$1 == "##" && $2 == v { n++ } END { print n + 0 }' "$changelog")
if [ "$count" -eq 0 ]; then
  echo "$changelog has no section for $version" >&2
  exit 1
elif [ "$count" -gt 1 ]; then
  echo "$changelog has $count sections for $version" >&2
  exit 1
fi

if [ "$mode" = title ]; then
  heading=$(awk -v v="$version" '$1 == "##" && $2 == v { sub(/^## +/, ""); print; exit }' "$changelog")
  printf 'Gono Forum %s\n' "$heading"
  exit 0
fi

# The section body, without the blank lines between the heading and the first text.
notes=$(awk -v v="$version" '
  /^## / { if (inside) exit; if ($1 == "##" && $2 == v) { inside = 1; next } }
  inside { print }
' "$changelog" | sed -e '/[^[:space:]]/,$!d')
[ -n "$notes" ] || {
  echo "the section for $version in $changelog has no text" >&2
  exit 1
}
printf '%s\n' "$notes"
