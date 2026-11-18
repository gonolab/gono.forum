#!/usr/bin/env bash
# Simulated runs of scripts/package-release.sh and scripts/release-notes.sh, in a temporary directory. package-release.sh
# runs from a copy of itself in a stand-in repository (forum.install.sh, the licence files, a crate version), with fakes
# of the build tools first on PATH: cargo (fetch, about, metadata, zigbuild), rustup, zig, cargo-zigbuild, cargo-about,
# npm, uname (Linux on x86_64), file and git. The fake build writes small stand-in binaries into the target directory
# that `cargo metadata` reports, which CARGO_TARGET_DIR moves as it moves Cargo's. Nothing outside the temporary
# directory is touched.
#
#   tests/release/simulate.sh          a few seconds; KEEP=1 keeps the temporary directory
#
# Needs bash, awk, sed, tar and sha256sum or shasum.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
sim=$(mktemp -d "${TMPDIR:-/tmp}/gonof-release.XXXXXX")
sim=$(cd "$sim" && pwd -P)
cleanup() { if [ -n "${KEEP:-}" ]; then echo "kept $sim"; else rm -rf "$sim"; fi; }
trap cleanup EXIT

real() { command -v "$1"; }
fakes=$sim/bin
mkdir -p "$fakes" "$sim/inject"

# shim NAME: writes a fake command from standard input, with @SIM@ and @REAL@ (the real command) filled in.
shim() {
  sed -e "s#@SIM@#$sim#g" -e "s#@REAL@#$(real "$1" || echo false)#g" > "$fakes/$1"
  chmod 0755 "$fakes/$1"
}

shim uname <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in -s) echo Linux ;; -m) echo x86_64 ;; *) exec @REAL@ "$@" ;; esac
EOF
shim git <<'EOF'
#!/usr/bin/env bash
echo 1700000000
EOF
for tool in rustup zig cargo-zigbuild cargo-about npm; do
  printf '#!/usr/bin/env bash\nexit 0\n' | shim "$tool"
done
# A stand-in binary describes itself to the fake file(1) with a line "# file: <description>".
shim file <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = -b ] && shift
sed -n 's/^# file: //p' "$1"
EOF
if ! command -v sha256sum >/dev/null; then
  shim sha256sum <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = -- ] && shift
exec shasum -a 256 "$@"
EOF
fi
# tar without the GNU options for reproducible archives where tar is not GNU tar (macOS).
shim tar <<'EOF'
#!/usr/bin/env bash
if @REAL@ --version 2>/dev/null | grep -q 'GNU tar'; then exec @REAL@ "$@"; fi
args=()
for arg; do
  case "$arg" in --sort=* | --owner=* | --group=* | --numeric-owner | --mtime=*) ;; *) args+=("$arg") ;; esac
done
exec @REAL@ "${args[@]}"
EOF
# Fake cargo. @SIM@/inject/NAME spoils one step: "fetch" makes cargo fetch fail (status 23), "build" makes zigbuild
# report success without writing anything, "wrong-arch" makes it write x86-64 binaries for every target.
shim cargo <<'EOF'
#!/usr/bin/env bash
target_dir=${CARGO_TARGET_DIR:-$PWD/target}
case "$1" in
  fetch)
    if [ -f @SIM@/inject/fetch ]; then rm -f @SIM@/inject/fetch; echo "error: simulated fetch failure" >&2; exit 23; fi ;;
  about)
    while [ $# -gt 0 ]; do [ "$1" = -o ] && { echo "third-party licences" > "$2"; break; }; shift; done ;;
  metadata)
    printf '{"packages":[],"target_directory":"%s","version":1,"workspace_root":"%s"}\n' "$target_dir" "$PWD" ;;
  zigbuild)
    triple=""
    while [ $# -gt 0 ]; do [ "$1" = --target ] && triple=$2; shift; done
    if [ -f @SIM@/inject/build ]; then rm -f @SIM@/inject/build; exit 0; fi
    case "$triple" in
      x86_64-*) machine=x86-64 ;;
      aarch64-*) machine="ARM aarch64" ;;
    esac
    if [ -f @SIM@/inject/wrong-arch ]; then machine=x86-64; fi
    mkdir -p "$target_dir/$triple/dist"
    for name in gono-forum-web gono-forum-cli; do
      printf '#!/bin/sh\n# file: ELF 64-bit LSB pie executable, %s, version 1 (SYSV), static-pie linked, stripped\necho "%s %s"\n' \
        "$machine" "$name" "$GONOF_BUILD_VERSION" > "$target_dir/$triple/dist/$name"
      chmod 0755 "$target_dir/$triple/dist/$name"
    done ;;
  *) echo "fake cargo: $1 is not simulated" >&2; exit 1 ;;
esac
EOF

# The stand-in repository: what package-release.sh reads, with the script itself copied in.
stand_in=$sim/repo
mkdir -p "$stand_in/scripts" "$stand_in/apps/web" "$stand_in/docs/licenses"
cp "$repo/scripts/package-release.sh" "$stand_in/scripts/"
printf '[package]\nname = "gono-forum-web"\nversion = "1.4.2"\n' > "$stand_in/apps/web/Cargo.toml"
for file in forum.install.sh LICENSE-MIT LICENSE-APACHE NOTICE docs/licenses/example.txt; do
  echo "stand-in $file" > "$stand_in/$file"
done
export PATH="$fakes:$PATH"
unset CARGO_TARGET_DIR

# ---------------------------------------------------------------------------------------------------------------
fail() {
  echo "FAIL: $*" >&2
  echo "--- output of the last run:" >&2
  cat "$sim/out" >&2
  exit 1
}
pass() { echo "ok   $*"; }
run() { # run EXPECTED_STATUS ARGS...: runs the stand-in's package-release.sh
  local expected=$1 status=0
  shift
  bash "$stand_in/scripts/package-release.sh" "$@" > "$sim/out" 2>&1 || status=$?
  [ "$status" -eq "$expected" ] || fail "expected exit status $expected, got $status"
}
says() { grep -qF -- "$1" "$sim/out" || fail "the output does not say: $1"; }
# packed ARCH: the text of the web binary in the package of ARCH under dist/1.4.2.
packed() {
  tar -xzOf "$stand_in/dist/1.4.2/gono-forum_1.4.2_linux_$1.tar.gz" "gono-forum_1.4.2_linux_$1/gono-forum-web"
}
leftovers() { find "$stand_in/dist" -mindepth 1 -maxdepth 1 -name '.*'; }

# Binaries of an earlier build, marked stale, in the repository's own target/: with CARGO_TARGET_DIR elsewhere, the
# packages must hold what the build wrote there.
for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  mkdir -p "$stand_in/target/$triple/dist"
  for name in gono-forum-web gono-forum-cli; do
    printf '#!/bin/sh\n# file: ELF 64-bit LSB pie executable, x86-64, static-pie linked\necho "%s stale"\n' "$name" \
      > "$stand_in/target/$triple/dist/$name"
  done
done
CARGO_TARGET_DIR=$sim/cargo-target run 0 --out "$stand_in/dist"
says "Packages are in $stand_in/dist/1.4.2"
for arch in amd64 arm64; do
  packed "$arch" | grep -q 'gono-forum-web 1.4.2' || fail "the $arch package does not hold the new build"
done
packed arm64 | grep -q 'ARM aarch64' || fail "the arm64 package holds another architecture"
(cd "$stand_in/dist/1.4.2" && sha256sum -c checksums.txt > /dev/null) || fail "checksums.txt does not match"
[ -z "$(leftovers)" ] || fail "temporary directories are left in dist/: $(leftovers)"
pass "the binaries come from Cargo's target directory, wherever CARGO_TARGET_DIR puts it"

cp "$stand_in/dist/1.4.2/checksums.txt" "$sim/checksums.before"
touch "$sim/inject/fetch"
run 23 --out "$stand_in/dist"
cmp -s "$sim/checksums.before" "$stand_in/dist/1.4.2/checksums.txt" || fail "the earlier packages are gone"
[ -f "$stand_in/dist/1.4.2/gono-forum_1.4.2_linux_amd64.tar.gz" ] || fail "the earlier amd64 package is gone"
[ -z "$(leftovers)" ] || fail "temporary directories are left in dist/: $(leftovers)"
pass "a rebuild that fails keeps the packages of the earlier build"

touch "$sim/inject/build"
CARGO_TARGET_DIR=$sim/cargo-target run 1 --arch amd64 --out "$stand_in/dist"
says "the build wrote no $sim/cargo-target/x86_64-unknown-linux-musl/dist/gono-forum-web"
cmp -s "$sim/checksums.before" "$stand_in/dist/1.4.2/checksums.txt" || fail "the earlier packages are gone"
pass "a build that writes no binaries does not pack the ones of an earlier build"

touch "$sim/inject/wrong-arch"
CARGO_TARGET_DIR=$sim/cargo-target run 1 --arch arm64 --out "$stand_in/dist"
rm -f "$sim/inject/wrong-arch"
says "gono-forum-web is not a Linux arm64 binary"
cmp -s "$sim/checksums.before" "$stand_in/dist/1.4.2/checksums.txt" || fail "the earlier packages are gone"
pass "a binary of another architecture is refused"

run 2 --version 1.4.2+build.7 --out "$stand_in/dist"
says "release versions take no build metadata: 1.4.2+build.7"
run 2 --version 1.4 --out "$stand_in/dist"
says "not a semantic version: 1.4"
pass "versions with build metadata are refused with the reason"

# ---------------------------------------------------------------------------------------------------------------
# release-notes.sh: headings inside fenced code blocks are part of the text.
changelog=$sim/CHANGELOG.md
cat > "$changelog" <<'EOF'
# Changelog

## 1.1.0

The first line.

```markdown
## Example heading
```

````text
```
## 1.0.0
```
````

~~~
## Not a heading either
~~~

Upgrade with forum.install.sh upgrade.

## 1.0.0 (early alpha)

The first release.
EOF
notes() { bash "$repo/scripts/release-notes.sh" "$@" "$changelog"; }
notes notes 1.1.0 > "$sim/out" || fail "release-notes.sh notes 1.1.0 failed"
says "## Example heading"
says "## Not a heading either"
says "Upgrade with forum.install.sh upgrade."
grep -qx 'The first line.' "$sim/out" || fail "the notes do not start with the section's text"
! grep -qF "The first release." "$sim/out" || fail "the notes run into the next section"
[ "$(notes title 1.0.0)" = "Gono Forum 1.0.0 (early alpha)" ] || fail "the title of 1.0.0 is $(notes title 1.0.0)"
[ "$(notes notes 1.0.0)" = "The first release." ] || fail "the notes of 1.0.0 are $(notes notes 1.0.0)"
pass "release notes keep fenced examples that contain headings"

echo "all simulated runs passed"
