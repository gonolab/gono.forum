#!/usr/bin/env bash
# Gono Forum installer: one instance per directory /opt/gono-forum-<instance>/ (binaries, config/, data/), run by a
# hardened systemd service as its own system account. First start opens the web setup wizard; this script prints
# the one-time token for it.
#
#   curl -fsSL https://gono.dev/forum.install.sh | sudo bash -s -- install
#
# install keeps a copy of this script, owned by root, in /usr/local/lib/gono-forum/; run the other commands from it:
#
#   sudo bash /usr/local/lib/gono-forum/forum.install.sh status
#
#   forum.install.sh install   [--instance NAME] [--port PORT | --listen ADDR] [--package FILE] [--checksums FILE]
#                        [--version VERSION] [--releases-url URL]
#   forum.install.sh upgrade   [--instance NAME] [--package FILE] [--checksums FILE] [--version VERSION] [--force]
#                        [--releases-url URL]
#   forum.install.sh rollback  [--instance NAME]
#   forum.install.sh uninstall [--instance NAME] [--purge [--yes]]
#   forum.install.sh status    [--instance NAME]
#
#   --instance      instance name (lowercase letters, digits, -; default: main)
#   --port          listen on 127.0.0.1:PORT (default 58833); --listen sets the full address
#   --package       install a local package instead of downloading; its checksums.txt must sit next to it or be given
#                   with --checksums, and must list the package
#   --version       a specific release instead of the latest
#   --releases-url  the release source (default: GitHub Releases of gonolab/gono.forum). install also writes it to
#                   the instance's configuration for the admin area's update check; upgrade needs it again, because
#                   this script does not take the source from a file the service account can change
#   --purge         also delete the configuration, the data and the system account (asks unless --yes)
#
# Every package and checksums file is verified with SHA-256; a missing or mismatching checksum stops the script.
# upgrade and rollback save every binary and its *.previous in switch-backup/ before they change any of them, and put
# all of them back when a step fails. An upgrade keeps the replaced binaries as *.previous and switches back by itself
# when the new version does not start or does not answer /healthz. uninstall keeps config/ and data/ unless --purge is
# given, and install over a kept config/ reinstalls the instance with its configuration, keys and data unchanged.
#
# shellcheck disable=SC2015,SC2016
# (SC2015: `a && b || die` means "die unless both worked". SC2016: the `sh -c` scripts expand their own arguments.)
set -euo pipefail

DEFAULT_RELEASES_URL="https://github.com/gonolab/gono.forum/releases"
BINARIES="gono-forum-web gono-forum-cli"
# Root runs this copy, and the unit's start guard runs it as the service account. Everything in the instance
# directory belongs to the service account, which can replace any entry there, so nothing root runs may live in it.
SCRIPT_DIR=/usr/local/lib/gono-forum
SCRIPT="$SCRIPT_DIR/forum.install.sh"

die() { echo "error: $*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
usage() { awk 'NR == 1 { next } /^#/ { print; next } { exit }' "$0" 2>/dev/null || true; }

# ---------------------------------------------------------------------------------------------------------------
# Switching binaries (upgrade, rollback and the start guard). Before the first binary changes, SAVE_SH puts a hard link
# to (or, on a file system without them, a copy of) every binary and its *.previous into switch-backup/, which appears
# with one rename once it is complete. Each binary is then replaced with one rename, so no name is ever missing. While
# switch-backup/ exists, RESTORE_SH makes the binaries what it holds again, whichever step stopped the switch; it can
# be run again when it stopped halfway itself. FORGET_SH renames switch-backup/ away: from then on the switch stands.
# The scripts run in the instance directory as the service account: root starts them through as_service, the start
# guard runs as that account already. The one-click upgrade refuses to run while switch-backup/ exists.
SAVED=switch-backup
# put SOURCE TARGET: makes TARGET a hard link to (or a copy of) SOURCE with one rename. It does nothing when TARGET
# already is SOURCE, which a rename would refuse.
PUT='put() { [ "$1" -ef "$2" ] && return 0; rm -f "$2.tmp" && { ln "$1" "$2.tmp" 2>/dev/null || cp "$1" "$2.tmp"; } && mv -fT "$2.tmp" "$2"; }
'
# SAVE_SH DIR SAVED BINARIES [MARKER]: saves the binaries; the start guard adds a copy of its upgrade marker, which tells
# its own switch-backup/ apart from one a command of this script left.
SAVE_SH=$PUT'
  set -e
  dir=$1 saved=$1/$2
  rm -rf "$saved.tmp" "$saved.done"
  mkdir -m 0700 "$saved.tmp"
  for name in $3; do
    for file in "$name" "$name.previous"; do
      if [ -f "$dir/$file" ]; then put "$dir/$file" "$saved.tmp/$file"; fi
    done
  done
  if [ -n "${4:-}" ]; then cp "$4" "$saved.tmp/pending"; fi
  mv -T "$saved.tmp" "$saved"'
# RESTORE_SH DIR SAVED KEEP_FAILED BINARIES: makes every binary and its *.previous what switch-backup/ holds (a name it
# does not hold is removed), drops staged *.new files, then removes switch-backup/. With KEEP_FAILED 1, binaries that
# differ from the saved ones are kept as *.failed first. A switch-backup/ without the first binary (gono-forum-web,
# which every switch starts from) is not one this script made, and is left alone.
RESTORE_SH=$PUT'
  set -e
  dir=$1 saved=$1/$2 failed=$3
  [ -d "$saved" ] || exit 0
  set -- $4
  if ! [ -f "$saved/$1" ]; then
    echo "$saved holds no $1, so it is not put back" >&2
    exit 1
  fi
  for name; do
    if [ "$failed" = 1 ] && [ -f "$dir/$name" ] && ! [ "$dir/$name" -ef "$saved/$name" ]; then
      put "$dir/$name" "$dir/$name.failed"
    fi
    for file in "$name" "$name.previous"; do
      if [ -f "$saved/$file" ]; then
        put "$saved/$file" "$dir/$file"
      else
        rm -f "$dir/$file"
      fi
    done
    rm -f "$dir/$name.new"
  done
  rm -rf "$saved.done"
  mv -T "$saved" "$saved.done"
  rm -rf "$saved.done" || true'
# FORGET_SH DIR SAVED
FORGET_SH='
  set -e
  saved=$1/$2
  rm -rf "$saved.done"
  mv -T "$saved" "$saved.done"
  rm -rf "$saved.done" || true'

# ---------------------------------------------------------------------------------------------------------------
# Start guard (ExecStartPre of the unit, runs as the service account). After a one-click upgrade marked
# data/upgrade/pending, the new version gets two starts; on the third the previous binaries are switched back, all of
# them or none. The new version removes the marker itself once it is serving.
#
# A rename that fails puts the upgraded binaries back, and the guard gives up: it removes the marker and lets the
# upgraded version keep starting rather than ever start a mix of both. It also gives up, changing nothing, when a
# *.previous is missing or switch-backup/ was left by a command of this script. A guard cut short (killed, power
# lost) left switch-backup/ with a copy of the marker: the next start puts that back and switches again.
guard() {
  local dir="$1" state n binary
  state="$dir/data/upgrade"
  [ -f "$state/pending" ] || return 0
  n=$(cat "$state/attempts" 2>/dev/null || echo 0)
  case "$n" in ''|*[!0-9]*) n=0 ;; esac
  n=$((n + 1))
  echo "$n" > "$state/attempts"
  [ "$n" -le 2 ] && return 0
  if [ -d "$dir/$SAVED" ]; then
    if ! [ -f "$dir/$SAVED/pending" ] || [ "$(cat "$dir/$SAVED/pending")" != "$(cat "$state/pending")" ]; then
      guard_give_up "$state" "$dir/$SAVED is left from an upgrade or rollback with forum.install.sh that did not finish (finish it with: forum.install.sh upgrade or rollback)"
      return 0
    fi
    if ! sh -c "$RESTORE_SH" sh "$dir" "$SAVED" 0 "$BINARIES"; then
      echo "gono-forum: could not put back the binaries an interrupted switch saved in $dir/$SAVED; the next start tries again" >&2
      return 1
    fi
  fi
  for binary in $BINARIES; do
    if ! [ -f "$dir/$binary.previous" ]; then
      guard_give_up "$state" "$dir/$binary.previous is missing"
      return 0
    fi
  done
  if ! sh -c "$SAVE_SH" sh "$dir" "$SAVED" "$BINARIES" "$state/pending"; then
    guard_give_up "$state" "could not save the upgraded binaries in $dir/$SAVED before switching"
    return 0
  fi
  # Each binary is kept as *.failed and its *.previous renamed over it.
  if ! sh -c "$PUT"'
      set -e
      dir=$1
      for name in $2; do
        put "$dir/$name" "$dir/$name.failed"
        mv -fT "$dir/$name.previous" "$dir/$name"
      done' sh "$dir" "$BINARIES"; then
    if sh -c "$RESTORE_SH" sh "$dir" "$SAVED" 0 "$BINARIES"; then
      guard_give_up "$state" "could not switch back to the previous binaries; the upgraded ones are in place again"
      return 0
    fi
    echo "gono-forum: could not switch back to the previous binaries, nor put the upgraded ones back; $dir/$SAVED holds them and the next start tries again" >&2
    return 1
  fi
  {
    grep -E '^(from|to|at)=' "$state/pending" || true
    echo "result=rolled_back"
    echo "finished=$(date +%s)"
  } > "$state/last.tmp"
  mv -f "$state/last.tmp" "$state/last"
  sh -c "$FORGET_SH" sh "$dir" "$SAVED" \
    || echo "gono-forum: could not remove $dir/$SAVED after switching back; forum.install.sh upgrade or rollback puts it back first" >&2
  rm -f "$state/pending" "$state/attempts"
  echo "gono-forum: the upgraded version failed to start twice; switched back to the previous binaries" >&2
}

# guard_give_up STATE REASON: no automatic switch back for this upgrade; the upgraded version stays.
guard_give_up() {
  rm -f "$1/pending" "$1/attempts"
  echo "gono-forum: $2; not switching back automatically, the upgraded version stays" >&2
}

if [ "${1:-}" = guard ]; then
  guard "${2:?guard needs the instance directory}" || exit 1
  exit 0
fi

# ---------------------------------------------------------------------------------------------------------------
command="${1:-}"
[ $# -gt 0 ] && shift
instance=main
listen=""
listen_given=0
port=58833
package=""
checksums=""
version=""
releases_url=""
force=0
purge=0
yes=0
while [ $# -gt 0 ]; do
  case "$1" in
    --instance) instance="${2:?--instance needs a value}"; shift 2 ;;
    --port) port="${2:?--port needs a value}"; listen_given=1; shift 2 ;;
    --listen) listen="${2:?--listen needs a value}"; listen_given=1; shift 2 ;;
    --package) package="${2:?--package needs a value}"; shift 2 ;;
    --checksums) checksums="${2:?--checksums needs a value}"; shift 2 ;;
    --version) version="${2:?--version needs a value}"; version="${version#v}"; shift 2 ;;
    --releases-url) releases_url="${2:?--releases-url needs a value}"; releases_url="${releases_url%/}"; shift 2 ;;
    --force) force=1; shift ;;
    --purge) purge=1; shift ;;
    --yes) yes=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

case "$command" in
  install|upgrade|rollback|uninstall|status) ;;
  ''|-h|--help|help) usage; echo "usage: forum.install.sh install|upgrade|rollback|uninstall|status [options]"; exit 0 ;;
  *) die "unknown command: $command (install, upgrade, rollback, uninstall or status)" ;;
esac

printf '%s' "$instance" | grep -Eq '^[a-z0-9][a-z0-9-]{0,20}$' \
  || die "instance names are 1-21 lowercase letters, digits or -, starting with a letter or digit"
name="gono-forum-$instance"
dir="/opt/$name"
unit="/etc/systemd/system/$name.service"
user="$name"

[ "$(id -u)" -eq 0 ] || die "run as root (sudo)"
[ "$(uname -s)" = Linux ] || die "Gono Forum packages are for Linux"
command -v systemctl >/dev/null && [ -d /run/systemd/system ] || die "systemd is required"
command -v setpriv >/dev/null && command -v setsid >/dev/null || die "setpriv and setsid (util-linux) are required"
command -v timeout >/dev/null || die "timeout (coreutils) is required"

case "$(uname -m)" in
  x86_64|amd64) arch=amd64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) die "no package for $(uname -m) (only x86_64 and arm64)" ;;
esac

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() {
  # fetch URL OUTPUT: HTTPS (or file:// for a local mirror), fails on any HTTP error.
  if command -v curl >/dev/null; then
    curl -fsSL --proto '=https,file' --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null; then
    case "$1" in https://*) ;; *) die "wget only fetches https:// addresses: $1" ;; esac
    wget -q -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

work=$(mktemp -d)
# The service account installs the verified binaries from here, so it must be able to read them.
chmod 0755 "$work"

# switching is 1 while switch-backup/ holds binaries that this run may still have to put back, stopped while this run
# has the service stopped. Whatever ends the script early (a failed command, an interrupt, a lost terminal), the exit
# handler puts the saved binaries back and starts the service again. A run killed outright leaves switch-backup/ for
# the next upgrade or rollback.
switching=0
stopped=0
on_exit() {
  local status=$?
  set +e
  trap '' INT TERM HUP
  if [ "$switching" -eq 1 ]; then
    echo "error: stopped while switching the binaries of $name; putting back the ones saved before" >&2
    if put_back 0 && comes_back; then
      echo "$name runs the binaries from before the switch again." >&2
    else
      echo "error: $name could not be brought back" >&2
      manual_help migrated
    fi
  elif [ "$stopped" -eq 1 ]; then
    start_service >/dev/null 2>&1 || echo "error: $name is stopped and did not start again; see: journalctl -u $name" >&2
  fi
  rm -rf "$work"
  exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# as_service COMMAND...: runs a command as the service account. Root never runs, writes, renames into or follows
# anything in the instance directory itself: that account owns it and could have put a link or a program of its own at
# any path there. The command gets a session of its own without a terminal, so it cannot type into root's shell.
as_service() {
  local uid gid
  uid=$(id -u "$user" 2>/dev/null) && gid=$(id -g "$user" 2>/dev/null) || die "the system account $user does not exist"
  (cd / && exec setsid -w setpriv --reuid="$uid" --regid="$gid" --clear-groups -- "$@")
}

# Text from files and programs the service account controls, made safe to print on root's terminal.
printable() { LC_ALL=C tr -cd '[:print:]\n'; }

# service_file PATH: a file in the instance directory, read as the service account; empty when it is missing or the
# account does not exist.
service_file() {
  [ -e "$1" ] && id "$user" >/dev/null 2>&1 || return 0
  as_service cat -- "$1" </dev/null 2>/dev/null || true
}

# PROBE COMMAND...: runs a binary to ask its version, killed when it does not answer within 15 seconds (as the
# one-click upgrade allows), so that a binary that hangs cannot hold up this script.
PROBE="timeout -k 5 15"

# binary_version FILE [NAME]: the version a binary in the instance directory prints after its NAME (gono-forum-web by
# default), run as the service account; empty when it does not run or does not answer in time.
binary_version() {
  # shellcheck disable=SC2086
  { as_service $PROBE "$1" --version </dev/null 2>/dev/null || true; } | sed -n "s/^${2:-gono-forum-web} //p" | printable
}

# Reads `releases_url = "..."` from the instance configuration, if any.
configured_releases_url() {
  service_file "$dir/config/gono-forum.toml" \
    | sed -n 's/^[[:space:]]*releases_url[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' | head -n 1 | printable
}

# Leaves the verified package unpacked in $work/package and sets $package_version.
obtain_package() {
  local file sums base listed expected configured
  if [ -n "$package" ]; then
    [ -f "$package" ] || die "no such package: $package"
    file="$package"
    sums="${checksums:-$(dirname "$package")/checksums.txt}"
    [ -f "$sums" ] || die "checksums file not found: $sums (put checksums.txt next to the package or pass --checksums)"
  else
    # The configuration's release source is for the admin area. This script installs what it downloads as root, its
    # own next version among it, so it takes a source other than the default only from the command line.
    if [ -z "$releases_url" ]; then
      configured=$(configured_releases_url)
      if [ -n "$configured" ] && [ "${configured%/}" != "$DEFAULT_RELEASES_URL" ]; then
        die "the instance configuration names the release source $configured; pass --releases-url to use it (it is not taken from a file the service account can change)"
      fi
    fi
    base="${releases_url:-$DEFAULT_RELEASES_URL}"
    sums="$work/checksums.txt"
    if [ -n "$version" ]; then
      fetch "$base/download/v$version/checksums.txt" "$sums" || die "cannot download checksums.txt of $version from $base"
    else
      fetch "$base/latest/download/checksums.txt" "$sums" || die "cannot download checksums.txt from $base"
    fi
    listed=$(grep -Eo "gono-forum_[0-9A-Za-z.+-]+_linux_${arch}\.tar\.gz" "$sums" | sort -u)
    [ -n "$version" ] && listed=$(printf '%s\n' "$listed" | grep -F "gono-forum_${version}_linux_${arch}.tar.gz" || true)
    [ "$(printf '%s\n' "$listed" | grep -c .)" -eq 1 ] || die "checksums.txt does not list exactly one linux_$arch package"
    file="$work/$listed"
    local tag
    tag=$(printf '%s' "$listed" | sed -n "s/^gono-forum_\(.*\)_linux_${arch}\.tar\.gz$/\1/p")
    fetch "$base/download/v$tag/$listed" "$file" || die "cannot download $listed"
  fi
  expected=$(awk -v name="$(basename "$file")" '{ sub(/^\*/, "", $2) } $2 == name { print tolower($1) }' "$sums" | head -n 1)
  [ -n "$expected" ] || die "$(basename "$file") is not listed in $sums"
  [ "$(sha256 "$file")" = "$expected" ] || die "SHA-256 of $(basename "$file") does not match $sums"
  mkdir -p "$work/package"
  tar -xzf "$file" -C "$work/package" --no-same-owner
  local found
  for binary in $BINARIES forum.install.sh; do
    found=$(find "$work/package" -maxdepth 2 -type f -name "$binary" | head -n 1)
    [ -n "$found" ] || die "the package has no $binary"
    mv "$found" "$work/$binary.unpacked"
  done
  chmod 0755 "$work"/*.unpacked
  # shellcheck disable=SC2086
  package_version=$($PROBE "$work/gono-forum-web.unpacked" --version </dev/null 2>/dev/null | sed -n 's/^gono-forum-web //p' || true)
  [ -n "$package_version" ] || die "the packaged gono-forum-web does not run on this machine"
}

installed_version() {
  binary_version "$dir/gono-forum-web"
}

# Puts the packaged copy of this script where root runs it from and the unit's start guard reads it.
install_script() {
  install -d -m 0755 -o root -g root "$SCRIPT_DIR"
  install -m 0755 -o root -g root "$work/forum.install.sh.unpacked" "$SCRIPT"
}

write_unit() {
  cat > "$unit.tmp" <<EOF
# Written by forum.install.sh; regenerated on upgrade. Local changes belong in a drop-in:
#   systemctl edit $name
[Unit]
Description=Gono Forum ($instance)
Documentation=https://github.com/gonolab/gono.forum
After=network-online.target mysql.service mysqld.service mariadb.service redis.service redis-server.service
Wants=network-online.target

[Service]
Type=notify
NotifyAccess=main
User=$user
Group=$user
WorkingDirectory=$dir
Environment=GONOF_HOME=$dir
Environment=RUST_LOG=gono_forum_web=info,tower_http=info
ExecStartPre=/bin/bash $SCRIPT guard $dir
ExecStart=$dir/gono-forum-web
Restart=always
RestartSec=3
TimeoutStartSec=120
TimeoutStopSec=30
UMask=0077
LimitNOFILE=65536
NoNewPrivileges=true
CapabilityBoundingSet=
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$dir
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
ProtectHostname=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
EOF
  mv -f "$unit.tmp" "$unit"
  systemctl daemon-reload
  # Earlier versions kept the script in the instance directory (named install.sh before that), and their units ran
  # the start guard from there.
  rm -f "$dir/forum.install.sh" "$dir/install.sh"
}

listen_address() {
  service_file "$dir/config/gono-forum.toml" \
    | sed -n 's/^[[:space:]]*listen[[:space:]]*=[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' | head -n 1 | printable
}

# Whether the started service answers: the forum (200) or the setup wizard (503 "setup required"). The forum also
# answers /healthz at the root when it is installed under a sub-path. A service that accepts connections but never
# answers must not hold up the switch back, so each request gets 2 seconds to connect and 5 in all, and the check
# gives up HEALTH_WAIT seconds after the first one. The address is local, so no proxy is asked.
HEALTH_WAIT=30
healthy() {
  local address code deadline limit
  address=$(listen_address)
  [ -n "$address" ] && command -v curl >/dev/null || return 0
  case "$address" in 0.0.0.0:*) address="127.0.0.1:${address##*:}" ;; '[::]':*) address="[::1]:${address##*:}" ;; esac
  deadline=$((SECONDS + HEALTH_WAIT))
  while :; do
    limit=$((deadline - SECONDS))
    [ "$limit" -le 5 ] || limit=5
    [ "$limit" -ge 1 ] || limit=1
    rm -f "$work/health"
    code=$(curl -s --noproxy '*' --connect-timeout 2 --max-time "$limit" -o "$work/health" -w '%{http_code}' \
      "http://$address/healthz" || true)
    [ "$code" = 200 ] && return 0
    [ "$code" = 503 ] && grep -q 'setup required' "$work/health" 2>/dev/null && return 0
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 1
  done
}

random_token() {
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
}

# write_private FILE: writes standard input to a file in the instance directory as the service account, mode 0600.
write_private() {
  as_service sh -c 'umask 077 && cat > "$1.tmp" && mv -f "$1.tmp" "$1"' sh "$1"
}

# ---------------------------------------------------------------------------------------------------------------
do_install() {
  [ ! -e "$dir/gono-forum-web" ] || die "$dir is already installed; use: bash $SCRIPT upgrade --instance $instance"
  [ ! -e "$unit" ] || die "$unit already exists"
  local config="$dir/config/gono-forum.toml" kept=0 created=0 token=""
  # A configuration kept by uninstall makes this a reinstall. It holds the database address and the three keys, so
  # it is kept as it is, and the setup wizard does not run again.
  if [ -e "$config" ]; then
    kept=1
    [ "$listen_given" -eq 0 ] || die "$config is kept from an earlier install and sets the listen address; change it there instead of passing --port or --listen"
    listen=$(listen_address)
  fi
  [ -n "$listen" ] || listen="127.0.0.1:$port"
  local listen_port="${listen##*:}"
  if command -v ss >/dev/null && [ -n "$(ss -ltnH "sport = :$listen_port" 2>/dev/null)" ]; then
    die "port $listen_port is already in use; choose another with --port"
  fi
  obtain_package
  if ! id "$user" >/dev/null 2>&1; then
    local nologin
    nologin=$(command -v nologin || echo /usr/sbin/nologin)
    useradd --system --user-group --no-create-home --home-dir "$dir" --shell "$nologin" "$user"
    created=1
  fi
  install -d -m 0750 -o "$user" -g "$user" "$dir"
  # Kept files that belonged to an account deleted since: nobody else can change them, so root can take them over.
  if [ "$created" -eq 1 ] && [ "$kept" -eq 1 ]; then
    chown -hR "$user:$user" "$dir/config" "$dir/data"
  fi
  as_service mkdir -p -m 0700 "$dir/config" "$dir/data"
  for binary in $BINARIES; do
    as_service install -m 0755 "$work/$binary.unpacked" "$dir/$binary"
  done
  install_script
  if [ "$kept" -eq 0 ]; then
    {
      echo "# Written by forum.install.sh. The setup wizard adds the database, Redis and the keys."
      echo "listen = \"$listen\""
      echo
      echo "[identity]"
      echo "local_directory = \"data\""
      echo
      echo "[media]"
      echo "local_directory = \"data/media\""
      echo
      echo "[redis]"
      echo "enabled = false"
      echo "namespace = \"gonof:$instance\""
      if [ -n "$releases_url" ]; then
        echo
        echo "[update]"
        echo "releases_url = \"$releases_url\""
      fi
    } | write_private "$config"
    token=$(random_token)
    printf '%s\n' "$token" | write_private "$dir/data/setup-token"
  else
    # A site whose setup was never finished still has its token.
    token=$(service_file "$dir/data/setup-token" | tr -cd '0-9a-f')
  fi
  write_unit
  systemctl enable --now "$name" >/dev/null 2>&1 || {
    journalctl -u "$name" -n 30 --no-pager >&2 || true
    die "the service did not start (see the log above)"
  }
  healthy || die "the service started but does not answer on $listen; see: journalctl -u $name"
  say ""
  if [ "$kept" -eq 1 ]; then
    say "Gono Forum $package_version is reinstalled as $name, with the configuration and data kept in $dir."
  else
    say "Gono Forum $package_version is installed as $name."
  fi
  say ""
  say "  Directory      $dir (binaries, config/, data/)"
  say "  Service        systemctl status $name    journalctl -u $name -f"
  say "  Manage         sudo bash $SCRIPT status --instance $instance    (also upgrade, rollback, uninstall)"
  say "  Listening on   $listen (local only; put your reverse proxy in front of it)"
  say ""
  if [ -n "$token" ]; then
    say "Finish the setup in the browser: open the forum's address (through your reverse proxy), or"
    say "http://$listen/setup on this machine, and enter the setup token:"
    say ""
    say "  $token"
    say ""
    say "The token works once and is also stored in $dir/data/setup-token."
  fi
  if [ "$kept" -eq 1 ] && [ -n "$releases_url" ]; then
    say "The kept configuration is unchanged: --releases-url was only used for this download."
  fi
  say "Reverse proxy (nginx): proxy_pass http://$listen; proxy_set_header Host \$host;"
  say "  proxy_set_header X-Real-IP \$remote_addr; proxy_set_header X-Forwarded-Proto \$scheme;"
}

# ---------------------------------------------------------------------------------------------------------------
# Switching binaries as root: the scripts are at the top of this file.
save_binaries() {
  as_service sh -c "$SAVE_SH" sh "$dir" "$SAVED" "$BINARIES"
}

# switch_to_new: each binary becomes its *.previous and the staged *.new takes its place.
switch_to_new() {
  as_service sh -c "$PUT"'
    set -e
    dir=$1
    for name in $2; do
      if [ -f "$dir/$name" ]; then put "$dir/$name" "$dir/$name.previous"; fi
      mv -fT "$dir/$name.new" "$dir/$name"
    done
  ' sh "$dir" "$BINARIES"
}

# switch_to_previous: each *.previous takes the place of its binary, which becomes the new *.previous.
switch_to_previous() {
  as_service sh -c "$PUT"'
    set -e
    dir=$1 saved=$1/$2
    for name in $3; do
      mv -fT "$dir/$name.previous" "$dir/$name"
      put "$saved/$name" "$dir/$name.previous"
    done
  ' sh "$dir" "$SAVED" "$BINARIES"
}

# restore_binaries KEEP_FAILED: see RESTORE_SH.
restore_binaries() {
  as_service sh -c "$RESTORE_SH" sh "$dir" "$SAVED" "$1" "$BINARIES"
}

forget_saved() {
  as_service sh -c "$FORGET_SH" sh "$dir" "$SAVED"
}

start_service() {
  stopped=0
  systemctl start "$name"
}

# put_back KEEP_FAILED: stops the service and puts back the binaries saved in switch-backup/. Fails when that did
# not work out; switch-backup/ is then kept.
put_back() {
  systemctl stop "$name" >/dev/null 2>&1 || true
  stopped=1
  restore_binaries "$1" || return 1
  switching=0
}

# comes_back: starts the service again after put_back and checks that it answers.
comes_back() {
  start_service && healthy && return 0
  journalctl -u "$name" -n 30 --no-pager >&2 || true
  return 1
}

# manual_help [migrated]: what to do by hand when the service did not come back, on standard error.
manual_help() {
  {
    echo "To do by hand:"
    echo "  Find the cause:    journalctl -u $name -n 100 --no-pager"
    if [ -d "$dir/$SAVED" ]; then
      echo "  The binaries from before are still saved in $dir/$SAVED;"
      echo "  the next upgrade or rollback puts them back first."
    fi
    if [ "${1:-}" = migrated ]; then
      echo "  If the newer version had already migrated the database, the older one may refuse it: restore the"
      echo "  database from your backup, then: systemctl restart $name"
    fi
    echo "  Check the result:  sudo bash $SCRIPT status --instance $instance"
  } >&2
}

# stranded MESSAGE [migrated]: the service could not be brought back; says so with what to do by hand, and stops.
stranded() {
  switching=0
  stopped=0
  echo "error: $1" >&2
  manual_help "${2:-}"
  exit 1
}

# finish_interrupted_switch: an upgrade or rollback killed while switching left switch-backup/ behind. Puts back what
# it saved and starts the service again before anything else happens.
finish_interrupted_switch() {
  [ -d "$dir/$SAVED" ] || return 0
  say "An earlier upgrade or rollback of $name stopped while switching binaries; putting back the ones saved before it."
  switching=1
  put_back 0 || stranded "could not put back the binaries saved in $dir/$SAVED"
  comes_back || stranded "the binaries from before the interrupted switch are back, but $name does not start or answer" migrated
  say "$name runs $(installed_version) again, as before the interrupted switch."
}

# runs_version VERSION: whether every installed binary reports VERSION, so that a retry does not take one replaced
# binary for a finished upgrade.
runs_version() {
  local binary
  for binary in $BINARIES; do
    [ "$(binary_version "$dir/$binary" "$binary")" = "$1" ] || return 1
  done
}

remove_staged() {
  local binary
  for binary in $BINARIES; do
    as_service rm -f "$dir/$binary.new" || true
  done
}

# stage_binaries: puts the verified binaries next to the installed ones as *.new and checks that each runs on this
# machine and reports the package's version. Nothing else changes; on failure the *.new files go again.
stage_binaries() {
  local binary reported
  for binary in $BINARIES; do
    if ! as_service install -m 0755 "$work/$binary.unpacked" "$dir/$binary.new"; then
      remove_staged
      die "could not write $dir/$binary.new; nothing was changed"
    fi
    reported=$(binary_version "$dir/$binary.new" "$binary")
    if [ "$reported" != "$package_version" ]; then
      remove_staged
      die "the packaged $binary reports version ${reported:-(none)} instead of $package_version; nothing was changed"
    fi
  done
}

record() {
  # record FROM TO RESULT: the outcome the admin area shows.
  printf 'from=%s\nto=%s\nat=%s\nresult=%s\nfinished=%s\n' "$1" "$2" "$(date +%s)" "$3" "$(date +%s)" \
    | as_service sh -c 'umask 077 && mkdir -p "$1" && cat > "$1/last.tmp" && mv -f "$1/last.tmp" "$1/last" && rm -f "$1/pending" "$1/attempts"' sh "$dir/data/upgrade"
}

do_upgrade() {
  [ -x "$dir/gono-forum-web" ] || die "$dir is not installed; use: forum.install.sh install --instance $instance"
  finish_interrupted_switch
  local current
  current=$(installed_version)
  [ -n "$current" ] || current=unknown
  obtain_package
  if [ "$force" -eq 0 ] && runs_version "$package_version"; then
    say "$name already runs $package_version (pass --force to reinstall it)."
    return 0
  fi
  stage_binaries
  install_script
  write_unit
  # A marker left by a one-click upgrade would make the start guard count the starts below as its own.
  as_service rm -f "$dir/data/upgrade/pending" "$dir/data/upgrade/attempts"
  if ! save_binaries; then
    remove_staged
    die "could not save the current binaries in $dir/$SAVED; nothing was changed"
  fi
  switching=1
  systemctl stop "$name"
  stopped=1
  if ! switch_to_new; then
    echo "error: could not put the new binaries in place; putting back $current" >&2
    put_back 0 || stranded "could not put back the binaries of $current"
    comes_back || stranded "the binaries of $current are back in place, but $name does not start or answer"
    die "upgrade to $package_version failed before it started; $name runs $current again"
  fi
  if start_service && healthy && forget_saved; then
    switching=0
    record "$current" "$package_version" upgraded || echo "warning: could not write $dir/data/upgrade/last" >&2
    say "$name upgraded from $current to $package_version. The previous binaries are kept as *.previous (bash $SCRIPT rollback)."
    return 0
  fi
  echo "error: $package_version did not start or does not answer on /healthz; switching back to $current" >&2
  journalctl -u "$name" -n 30 --no-pager >&2 || true
  put_back 1 || stranded "could not put back the binaries of $current" migrated
  record "$current" "$package_version" rolled_back || echo "warning: could not write $dir/data/upgrade/last" >&2
  comes_back || stranded "the binaries of $current are back in place (the new ones are kept as *.failed), but $current does not start or answer either" migrated
  die "upgrade to $package_version failed; $name runs $current again (the new binaries are kept as *.failed)"
}

do_rollback() {
  if [ -d "$dir/$SAVED" ]; then
    finish_interrupted_switch
    say "Nothing else was changed; run rollback again to switch to the previous binaries."
    return 0
  fi
  [ -f "$dir/gono-forum-web.previous" ] || die "no previous version to roll back to in $dir"
  local binary current previous version
  current=$(installed_version)
  [ -n "$current" ] || current=unknown
  previous=$(binary_version "$dir/gono-forum-web.previous")
  [ -n "$previous" ] || die "$dir/gono-forum-web.previous does not report a version; nothing was changed"
  for binary in $BINARIES; do
    [ -f "$dir/$binary.previous" ] || die "$dir/$binary.previous is missing, so the previous version is incomplete; nothing was changed"
    version=$(binary_version "$dir/$binary.previous" "$binary")
    [ "$version" = "$previous" ] \
      || die "the previous binaries are not one version (gono-forum-web $previous, $binary ${version:-unknown}); nothing was changed"
  done
  save_binaries || die "could not save the current binaries in $dir/$SAVED; nothing was changed"
  switching=1
  systemctl stop "$name"
  stopped=1
  if ! switch_to_previous; then
    echo "error: could not switch the binaries; putting back $current" >&2
    put_back 0 || stranded "could not put back the binaries of $current"
    comes_back || stranded "the binaries of $current are back in place, but $name does not start or answer"
    die "rollback failed; nothing was changed and $name runs $current again"
  fi
  # A rollback asked for by hand stands even when the previous version does not start: running it again switches
  # forward.
  forget_saved
  switching=0
  record "$current" "$previous" switched || echo "warning: could not write $dir/data/upgrade/last" >&2
  comes_back || stranded "$previous does not start or answer either; $current is kept as *.previous (rolling back again switches forward)" migrated
  say "$name runs $previous again; $current is kept as *.previous (rolling back again switches forward)."
}

do_uninstall() {
  [ -e "$unit" ] || [ -d "$dir" ] || die "$name is not installed"
  if [ "$purge" -eq 1 ] && [ "$yes" -eq 0 ]; then
    { : < /dev/tty; } 2>/dev/null || die "--purge deletes $dir with its configuration and data; confirm with --yes"
    printf 'Delete %s with its configuration and data, and the account %s? Type the instance name to confirm: ' "$dir" "$user" > /dev/tty
    local answer
    read -r answer < /dev/tty
    [ "$answer" = "$instance" ] || die "not confirmed; nothing was removed"
  fi
  systemctl disable --now "$name" >/dev/null 2>&1 || true
  rm -f "$unit"
  systemctl daemon-reload
  if [ "$purge" -eq 1 ]; then
    rm -rf "$dir"
    id "$user" >/dev/null 2>&1 && userdel "$user" || true
    # The last instance takes the shared copy of this script with it.
    if ! compgen -G "/opt/gono-forum-*" >/dev/null && ! compgen -G "/etc/systemd/system/gono-forum-*.service" >/dev/null; then
      rm -rf "$SCRIPT_DIR"
    fi
    say "$name is removed, with its configuration, data and account."
  else
    local binary
    for binary in $BINARIES; do
      rm -f "$dir/$binary" "$dir/$binary.previous" "$dir/$binary.new" "$dir/$binary.failed" \
        "$dir/$binary.tmp" "$dir/$binary.previous.tmp" "$dir/$binary.failed.tmp"
    done
    rm -rf "$dir/${SAVED:?}" "$dir/$SAVED.tmp" "$dir/$SAVED.done"
    rm -f "$dir/forum.install.sh" "$dir/install.sh"
    say "$name is removed. The configuration and data are kept in $dir/config and $dir/data (and the account $user)."
    say "Installing the instance again keeps them; delete them with: bash $SCRIPT uninstall --instance $instance --purge"
  fi
  say "A database server, Redis and reverse proxy configuration you set up yourself are not touched."
}

do_status() {
  [ -d "$dir" ] || die "$name is not installed"
  local version cli
  version=$(installed_version)
  cli=$(binary_version "$dir/gono-forum-cli" gono-forum-cli)
  say "Instance   $instance ($dir)"
  if [ "$cli" = "$version" ]; then
    say "Version    ${version:-unknown}"
  else
    say "Version    ${version:-unknown}, but gono-forum-cli ${cli:-does not run} (upgrade again to put both in place)"
  fi
  [ -f "$dir/gono-forum-web.previous" ] && say "Previous   $(binary_version "$dir/gono-forum-web.previous")"
  say "Listen     $(listen_address)"
  say "Service    $(systemctl is-active "$name" 2>/dev/null || true)"
  [ -d "$dir/$SAVED" ] && say "Unfinished an upgrade or rollback stopped while switching binaries; the next upgrade or rollback puts back the ones saved in $dir/$SAVED"
  [ -f "$dir/data/setup-token" ] && say "Setup      waiting for the setup wizard (token in $dir/data/setup-token)"
  if [ -f "$dir/data/upgrade/last" ]; then
    say "Last upgrade:"
    service_file "$dir/data/upgrade/last" | printable | sed 's/^/  /'
  fi
  return 0
}

"do_$command"
