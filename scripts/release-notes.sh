#!/usr/bin/env bash
# The GitHub release title and text of a version, taken from its section in CHANGELOG.md
# (.github/workflows/release.yml; docs/releasing.md, "Publishing a release").
#
#   scripts/release-notes.sh title <version> [changelog]   "Gono Forum " and the section heading without "## "
#   scripts/release-notes.sh notes <version> [changelog]   the section without its heading
#
# A section starts at a line "## <version>", optionally followed by a space and a label such as "(early alpha)", and
# ends before the next line that starts with "## ". Lines inside fenced code blocks (``` or ~~~) are never headings, so
# an example in the notes can show one. The script fails when CHANGELOG.md has no section for the version, more than
# one, or one without text, so that a release is never published without notes.
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

# HEADINGS: awk functions that tell headings from lines of fenced code blocks. Call fence() for every line; heading()
# is true for a line that starts with "## " outside a code block. A fence is three or more backticks or tildes after at
# most three spaces; the block ends at a fence of the same character, at least as long, with nothing but spaces after.
# (No interval expressions such as {3,}: mawk, the awk of Ubuntu, may not have them.)
HEADINGS='
  function run(line,    c, n) {
    sub(/^ ? ? ?/, "", line)
    c = substr(line, 1, 1)
    if (c != "`" && c != "~") return ""
    n = 0
    while (substr(line, n + 1, 1) == c) n++
    return n >= 3 ? substr(line, 1, n) : ""
  }
  function fence(line,    r, rest) {
    r = run(line)
    if (r == "") return
    if (fenced == "") { fenced = r; return }
    rest = line
    sub(/^ ? ? ?/, "", rest)
    rest = substr(rest, length(r) + 1)
    if (substr(r, 1, 1) == substr(fenced, 1, 1) && length(r) >= length(fenced) && rest ~ /^[ \t]*$/) fenced = ""
  }
  function heading() { return fenced == "" && /^## / }
'

# Exact field comparison rather than a regular expression, so the dots in the version match only dots.
count=$(awk -v v="$version" "$HEADINGS"'
  { fence($0) }
  heading() && $1 == "##" && $2 == v { n++ }
  END { print n + 0 }
' "$changelog")
if [ "$count" -eq 0 ]; then
  echo "$changelog has no section for $version" >&2
  exit 1
elif [ "$count" -gt 1 ]; then
  echo "$changelog has $count sections for $version" >&2
  exit 1
fi

if [ "$mode" = title ]; then
  heading=$(awk -v v="$version" "$HEADINGS"'
    { fence($0) }
    heading() && $1 == "##" && $2 == v { sub(/^## +/, ""); print; exit }
  ' "$changelog")
  printf 'Gono Forum %s\n' "$heading"
  exit 0
fi

# The section body, without the blank lines between the heading and the first text.
notes=$(awk -v v="$version" "$HEADINGS"'
  { fence($0) }
  heading() { if (inside) exit; if ($1 == "##" && $2 == v) { inside = 1; next } }
  inside { print }
' "$changelog" | sed -e '/[^[:space:]]/,$!d')
[ -n "$notes" ] || {
  echo "the section for $version in $changelog has no text" >&2
  exit 1
}
printf '%s\n' "$notes"
