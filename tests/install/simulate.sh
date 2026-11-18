#!/usr/bin/env bash
# Simulated runs of forum.install.sh: install, upgrade, rollback and their failure paths, against a fake system in a
# temporary directory. Nothing outside that directory is touched. The script under test is a copy whose system paths
# (/opt, /etc/systemd/system, /usr/local/lib/gono-forum, /run/systemd/system) point into the temporary directory, and
# the commands that would reach the real system (systemctl, journalctl, id, uname, setpriv, setsid, useradd, userdel,
# chown, the owner options of install) are small fakes put first on PATH. The packages hold a fake gono-forum-web: a
# shell script around a tiny Python server on a free loopback port that, per version, answers /healthz, answers it
# with an error or as the setup wizard does, exits before it is ready, or accepts connections and never answers. Runs
# without curl (and without wget) check the health check's fallbacks; the wget ones are skipped when it is missing.
#
#   tests/install/simulate.sh          about four minutes; KEEP=1 keeps the temporary directory (with transcript, the
#                                      output of every run)
#
# Needs bash, curl, python3, tar and GNU coreutils (on macOS: brew install coreutils; found by itself). Runs as an
# ordinary user and refuses to run as root.
set -euo pipefail

[ "$(id -u)" -ne 0 ] || { echo "run this as an ordinary user, not as root" >&2; exit 1; }
for tool in curl python3 tar; do
  command -v "$tool" >/dev/null || { echo "needs $tool" >&2; exit 1; }
done
gnubin=""
if ! mv --version 2>/dev/null | grep -q GNU; then
  for candidate in /opt/homebrew/opt/coreutils/libexec/gnubin /usr/local/opt/coreutils/libexec/gnubin; do
    if [ -x "$candidate/mv" ]; then gnubin=$candidate; break; fi
  done
  [ -n "$gnubin" ] || { echo "needs GNU coreutils (on macOS: brew install coreutils)" >&2; exit 1; }
fi
base_path=${gnubin:+$gnubin:}$PATH
real() { PATH=$base_path command -v "$1"; }

repo=$(cd "$(dirname "$0")/../.." && pwd)
sim=$(mktemp -d "${TMPDIR:-/tmp}/gonof-install.XXXXXX")
sim=$(cd "$sim" && pwd -P)
case "$sim" in *[!A-Za-z0-9/._-]*) echo "the temporary directory has unusual characters: $sim" >&2; exit 1 ;; esac
root=$sim/root

cleanup() {
  local file
  for file in "$sim"/systemd/*.pid; do
    if [ -f "$file" ]; then kill -KILL "$(cat "$file")" 2>/dev/null || true; fi
  done
  if [ -n "${KEEP:-}" ]; then echo "kept $sim"; else rm -rf "$sim"; fi
}
trap cleanup EXIT

mkdir -p "$root/opt" "$root/etc/systemd/system" "$root/usr/local/lib" "$root/run/systemd/system" \
  "$sim/bin" "$sim/systemd" "$sim/behavior" "$sim/pkg" "$sim/inject" "$sim/tmp"
touch "$sim/users"

# The script under test, with every system path moved into the simulation.
script=$sim/forum.install.sh
sed -e "s#/opt/#$root/opt/#g" \
  -e "s#/etc/systemd/system#$root/etc/systemd/system#g" \
  -e "s#/usr/local/lib/gono-forum#$root/usr/local/lib/gono-forum#g" \
  -e "s#/run/systemd/system#$root/run/systemd/system#g" \
  "$repo/forum.install.sh" > "$script"
if grep -v '^[[:space:]]*#' "$script" | grep -nE '(^|[^A-Za-z0-9_./-])/(opt|etc/systemd|usr/local/lib|run/systemd)/'; then
  echo "forum.install.sh names a system path this simulation does not move (above)" >&2
  exit 1
fi

# shim NAME: writes a fake command from standard input, with @SIM@, @ROOT@ and @REAL@ (the real command) filled in.
shim() {
  sed -e "s#@SIM@#$sim#g" -e "s#@ROOT@#$root#g" -e "s#@REAL@#$(real "$1" || echo false)#g" > "$sim/bin/$1"
  chmod 0755 "$sim/bin/$1"
}

shim id <<'EOF'
#!/usr/bin/env bash
# The simulation runs as root, and accounts made by the fake useradd exist (as the current user).
known() { grep -qxF -- "$1" @SIM@/users; }
case "${1:-}" in
  -u|-g)
    if [ $# -eq 1 ]; then echo 0; exit 0; fi
    known "$2" || { echo "id: '$2': no such user" >&2; exit 1; }
    exec @REAL@ "$1" ;;
  -*) exec @REAL@ "$@" ;;
  '') exec @REAL@ ;;
  *) known "$1" || { echo "id: '$1': no such user" >&2; exit 1; } ;;
esac
EOF
shim uname <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in -s) echo Linux ;; -m) echo x86_64 ;; *) exec @REAL@ "$@" ;; esac
EOF
shim setsid <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = -w ] && shift
exec "$@"
EOF
shim setpriv <<'EOF'
#!/usr/bin/env bash
while [ $# -gt 0 ] && [ "$1" != -- ]; do shift; done
shift
exec "$@"
EOF
shim useradd <<'EOF'
#!/usr/bin/env bash
for name; do :; done
echo "$name" >> @SIM@/users
EOF
shim userdel <<'EOF'
#!/usr/bin/env bash
# @SIM@/inject/userdel makes the next call fail.
if [ -f @SIM@/inject/userdel ]; then
  rm -f @SIM@/inject/userdel
  echo "userdel: user $1 is currently used by process 1 (simulated)" >&2
  exit 8
fi
grep -vxF -- "$1" @SIM@/users > @SIM@/users.tmp || true
mv -f @SIM@/users.tmp @SIM@/users
EOF
shim chown <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
shim install <<'EOF'
#!/usr/bin/env bash
# install without -o and -g, which only root may use. "fail NAME" in @SIM@/inject/install makes the next install of a
# file named NAME fail.
args=()
while [ $# -gt 0 ]; do
  case "$1" in -o|-g) shift 2 ;; *) args+=("$1"); shift ;; esac
done
if [ -f @SIM@/inject/install ]; then
  read -r action target < @SIM@/inject/install
  if [ "$action" = fail ] && [ "${args[${#args[@]}-1]##*/}" = "$target" ]; then
    rm -f @SIM@/inject/install
    echo "install: cannot create regular file '$target': No space left on device (simulated)" >&2
    exit 1
  fi
fi
exec @REAL@ "${args[@]}"
EOF
shim mv <<'EOF'
#!/usr/bin/env bash
# GNU mv, except that @SIM@/inject/mv can spoil one call: "fail NAME" makes the next move of a file named NAME fail,
# "kill NAME" first kills the forum.install.sh run (pid in @SIM@/script.pid) with SIGKILL, as a power cut would, and
# "cut NAME" kills the script that called mv and the one that started it (the start guard) the same way.
if [ -f @SIM@/inject/mv ]; then
  read -r action target < @SIM@/inject/mv
  source=""
  for arg; do
    case "$arg" in -*) continue ;; esac
    source=$arg
    break
  done
  if [ "${source##*/}" = "$target" ]; then
    rm -f @SIM@/inject/mv
    [ "$action" = kill ] && kill -KILL "$(cat @SIM@/script.pid)"
    [ "$action" = cut ] && kill -KILL "$(ps -o ppid= -p "$PPID" | tr -d ' ')" "$PPID"
    echo "mv: cannot move '$source': Input/output error (simulated)" >&2
    exit 1
  fi
fi
exec @REAL@ "$@"
EOF
shim journalctl <<'EOF'
#!/usr/bin/env bash
name=""
while [ $# -gt 0 ]; do
  case "$1" in -u) name=${2%.service}; shift 2 ;; *) shift ;; esac
done
[ -f "@SIM@/systemd/$name.log" ] && tail -n 30 "@SIM@/systemd/$name.log"
exit 0
EOF
shim systemctl <<'EOF'
#!/usr/bin/env bash
# Fake systemd: units come from @ROOT@/etc/systemd/system; a started service runs in the background with its pid in
# @SIM@/systemd/<name>.pid and its output in <name>.log. Like Type=notify, start waits until the service says it is
# ready (the fake forum creates $NOTIFY_READY) and fails when it exits first or takes longer than 10 seconds.
# @SIM@/inject/systemctl spoils the next disable: "fail disable" makes it fail, "ignore disable" makes it report
# success while the service keeps running.
state=@SIM@/systemd
units=@ROOT@/etc/systemd/system

value() { sed -n "s/^$2=//p" "$units/$1.service"; }
running() { [ -f "$state/$1.pid" ] && kill -0 "$(cat "$state/$1.pid")" 2>/dev/null; }

start() {
  local name=$1 home dir pre exe pid _
  running "$name" && return 0
  [ -f "$units/$name.service" ] || { echo "Failed to start $name.service: Unit $name.service not found." >&2; return 5; }
  dir=$(value "$name" WorkingDirectory)
  home=$(value "$name" Environment | sed -n 's/^GONOF_HOME=//p')
  pre=$(value "$name" ExecStartPre)
  exe=$(value "$name" ExecStart)
  echo "--- systemctl start $name" >> "$state/$name.log"
  # shellcheck disable=SC2086
  if [ -n "$pre" ] && ! (cd "$dir" && GONOF_HOME=$home $pre) >> "$state/$name.log" 2>&1 < /dev/null; then
    echo "Job for $name.service failed because the control process exited with error code." >&2
    return 1
  fi
  rm -f "$state/$name.ready"
  (cd "$dir" && exec env GONOF_HOME="$home" NOTIFY_READY="$state/$name.ready" "$exe") >> "$state/$name.log" 2>&1 < /dev/null &
  pid=$!
  echo "$pid" > "$state/$name.pid"
  for _ in $(seq 100); do
    [ -f "$state/$name.ready" ] && return 0
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL "$pid" 2>/dev/null
  rm -f "$state/$name.pid"
  echo "Job for $name.service failed because the control process exited with error code." >&2
  return 1
}

stop() {
  local name=$1 pid _
  if running "$name"; then
    pid=$(cat "$state/$name.pid")
    kill -TERM "$pid" 2>/dev/null
    for _ in $(seq 50); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL "$pid" 2>/dev/null
  fi
  rm -f "$state/$name.pid" "$state/$name.ready"
  return 0
}

command=$1
shift
now=0 unit=""
for arg; do
  case "$arg" in --now) now=1 ;; -*) ;; *) unit=${arg%.service} ;; esac
done
if [ -f @SIM@/inject/systemctl ]; then
  read -r action target < @SIM@/inject/systemctl
  if [ "$target" = "$command" ]; then
    rm -f @SIM@/inject/systemctl
    [ "$action" = ignore ] && exit 0
    echo "Failed to disable unit: Access denied (simulated)" >&2
    exit 1
  fi
fi
case "$command" in
  daemon-reload) ;;
  start) start "$unit" ;;
  stop) stop "$unit" ;;
  restart) stop "$unit"; start "$unit" ;;
  enable) [ "$now" -eq 0 ] || start "$unit" ;;
  disable) [ "$now" -eq 0 ] || stop "$unit" ;;
  is-active) if running "$unit"; then echo active; else echo inactive; exit 3; fi ;;
  *) echo "fake systemctl: $command is not simulated" >&2; exit 1 ;;
esac
EOF

cat > "$sim/server.py" <<'EOF'
# Fake forum: listens on the address in argv[1]; "ok" answers every request with "ok <version>", "error" with a 500,
# "setup" with the setup wizard's 503 "setup required", and "hang" accepts connections and never answers. Creates
# argv[4] once it listens, as the real one sends READY=1.
import socket, sys, threading

host, port = sys.argv[1].rsplit(":", 1)
behavior, version, ready = sys.argv[2], sys.argv[3], sys.argv[4]
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind((host, int(port)))
server.listen(16)
open(ready, "w").close()


def answer(connection):
    with connection:
        if behavior == "hang":
            threading.Event().wait()
        connection.recv(65536)
        status, body = {
            "error": ("500 Internal Server Error", "internal error"),
            "setup": ("503 Service Unavailable", "setup required"),
        }.get(behavior, ("200 OK", f"ok {version}"))
        body = f"{body}\n".encode()
        head = f"HTTP/1.1 {status}\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
        connection.sendall(head + body)


while True:
    connection, _ = server.accept()
    threading.Thread(target=answer, args=(connection,), daemon=True).start()
EOF

python=$(command -v python3)
# make_package VERSION: builds gono-forum_VERSION_linux_amd64.tar.gz with checksums.txt and prints its path. How the
# fake forum of VERSION behaves when started is read from $sim/behavior/VERSION (ok, error, setup, crash or hang; ok if
# missing);
# "hang" in $sim/behavior/VERSION-web-version or VERSION-cli-version makes that binary's --version hang.
make_package() {
  local version=$1 package stage
  package=gono-forum_${version}_linux_amd64
  stage=$sim/pkg/$version/$package
  mkdir -p "$stage"
  cat > "$stage/gono-forum-web" <<EOF
#!/usr/bin/env bash
# Fake gono-forum-web $version.
if [ "\${1:-}" = --version ]; then
  [ "\$(cat "$sim/behavior/$version-web-version" 2>/dev/null)" = hang ] && exec sleep 3600
  echo "gono-forum-web $version"
  exit 0
fi
behavior=\$(cat "$sim/behavior/$version" 2>/dev/null || echo ok)
echo "gono-forum-web $version starting: \$behavior"
if [ "\$behavior" = crash ]; then echo "gono-forum-web $version: exits before it is ready" >&2; exit 1; fi
listen=\$(sed -n -e 's/^listen = "\\([^"]*\\)".*/\\1/p' -e "s/^listen = '\\([^']*\\)'.*/\\1/p" "\$GONOF_HOME/config/gono-forum.toml" | head -n 1)
exec "$python" -I "$sim/server.py" "\$listen" "\$behavior" "$version" "\${NOTIFY_READY:-/dev/null}"
EOF
  cat > "$stage/gono-forum-cli" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = --version ]; then
  [ "\$(cat "$sim/behavior/$version-cli-version" 2>/dev/null)" = hang ] && exec sleep 3600
  echo "gono-forum-cli $version"
  exit 0
fi
echo "fake gono-forum-cli $version"
EOF
  cp "$script" "$stage/forum.install.sh"
  chmod 0755 "$stage"/*
  (cd "$sim/pkg/$version" && tar -czf "$package.tar.gz" "$package" && sha256sum "$package.tar.gz" > checksums.txt)
  echo "$sim/pkg/$version/$package.tar.gz"
}

export PATH="$sim/bin:$base_path" TMPDIR="$sim/tmp"
port=$("$python" -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
dir=$root/opt/gono-forum-main
pkg1=$(make_package 0.1.0)
pkg2=$(make_package 0.2.0)
pkg3=$(make_package 0.3.0)
make_package 0.4.0 > /dev/null
pkg5=$(make_package 0.5.0)
pkg6=$(make_package 0.6.0)
pkg7=$(make_package 0.7.0)

# without NAMES...: prints a directory to use as PATH that has every command of the simulation's PATH except NAMES
# (as links, in the same order of precedence), for runs of the script under test without curl or wget.
without() {
  local into="$sim/path-without" name entry file
  for name; do into="$into-$name"; done
  if ! [ -d "$into" ]; then
    mkdir -p "$into"
    local IFS=:
    for entry in $PATH; do
      for file in "$entry"/*; do
        name=${file##*/}
        case " $* " in *" $name "*) continue ;; esac
        if [ -x "$file" ] && ! [ -d "$file" ] && ! [ -e "$into/$name" ]; then ln -s "$file" "$into/$name"; fi
      done
    done
  fi
  echo "$into"
}
# set_listen LINE: makes LINE the listen line of the instance configuration, at its top (removes it when LINE is
# empty).
set_listen() {
  local config=$dir/config/gono-forum.toml
  awk -v line="$1" 'NR == 1 && line != "" { print line } /^listen[ \t]*=/ { next } { print }' "$config" > "$config.edit"
  mv -f "$config.edit" "$config"
}
# ---------------------------------------------------------------------------------------------------------------
status=0
fail() {
  echo "FAIL: $*" >&2
  echo "--- output of the last run:" >&2
  cat "$sim/out" >&2
  echo "--- end of the service log:" >&2
  tail -n 20 "$sim/systemd/gono-forum-main.log" >&2 || true
  exit 1
}
start_run() { # start_run ARGS...: starts the script under test in the background
  echo "forum.install.sh $*" > "$sim/command"
  bash "$script" "$@" > "$sim/out" 2>&1 &
  echo $! > "$sim/script.pid"
}
finish_run() { # finish_run EXPECTED_STATUS: waits for it and checks how it ended; every run's output goes to transcript
  status=0
  # (No job notice from bash for the run killed on purpose.)
  { wait "$(cat "$sim/script.pid")"; } 2>/dev/null || status=$?
  { echo "=== $(cat "$sim/command") (exit $status)"; cat "$sim/out"; } >> "$sim/transcript"
  [ "$status" -eq "$1" ] || fail "expected exit status $1, got $status"
}
run() { # run EXPECTED_STATUS ARGS...
  local expected=$1
  shift
  start_run "$@"
  finish_run "$expected"
}
says() { grep -qF -- "$1" "$sim/out" || fail "the output does not say: $1"; }
never_says() { ! grep -qF -- "$1" "$sim/out" || fail "the output says: $1"; }
reports() { # reports FILE VERSION: the fake binary FILE prints VERSION
  local got
  got=$("$1" --version 2>/dev/null | sed 's/^[^ ]* //') || true
  [ "$got" = "$2" ] || fail "${1#"$dir"/} reports '${got:-nothing}', expected $2"
}
binaries() { # binaries SUFFIX VERSION: both binaries with SUFFIX ("" or .previous, .failed) report VERSION
  reports "$dir/gono-forum-web$1" "$2"
  reports "$dir/gono-forum-cli$1" "$2"
}
absent() {
  local path
  for path; do [ ! -e "$path" ] || fail "${path#"$dir"/} should not exist"; done
}
no_leftovers() {
  absent "$dir/switch-backup" "$dir/switch-backup.tmp" "$dir/switch-backup.done" \
    "$dir"/gono-forum-*.new "$dir"/gono-forum-*.tmp
}
serving() { # serving VERSION|nothing: what /healthz answers now
  local got
  got=$(curl -s --max-time 2 "http://127.0.0.1:$port/healthz" | sed 's/^ok //') || true
  [ "${got:-nothing}" = "$1" ] || fail "/healthz answers '${got:-nothing}', expected $1"
}
last_result() {
  grep -qx "result=$1" "$dir/data/upgrade/last" || fail "data/upgrade/last does not say result=$1"
}
pass() { echo "ok   $*"; }
logged() { grep -qF -- "$1" "$sim/systemd/gono-forum-main.log" || fail "the service log does not say: $1"; }
# one_click VERSION: what a one-click upgrade (updates::install) does to the files before the forum exits, then the
# exit. Its starts are left to the test, as systemd would make them.
one_click() {
  local version=$1 stage=$sim/pkg/$1/gono-forum_$1_linux_amd64 from name
  from=$("$dir/gono-forum-web" --version | sed 's/^[^ ]* //')
  for name in gono-forum-web gono-forum-cli; do cp "$stage/$name" "$dir/$name.new"; done
  mkdir -p "$dir/data/upgrade"
  printf 'from=%s\nto=%s\nat=%s\n' "$from" "$version" "$(date +%s)" > "$dir/data/upgrade/pending"
  rm -f "$dir/data/upgrade/attempts"
  for name in gono-forum-web gono-forum-cli; do
    mv -fT "$dir/$name" "$dir/$name.previous"
    mv -fT "$dir/$name.new" "$dir/$name"
  done
  systemctl stop gono-forum-main
  : > "$sim/systemd/gono-forum-main.log"
}
start_fails() { ! systemctl start gono-forum-main 2> /dev/null || fail "gono-forum-main should not have started"; }
start_works() { systemctl start gono-forum-main 2> /dev/null || fail "gono-forum-main should have started"; }
no_marker() { absent "$dir/data/upgrade/pending" "$dir/data/upgrade/attempts"; }

# ---------------------------------------------------------------------------------------------------------------
# An install that stops after writing both binaries: the instance has no unit and no configuration, so it is not
# installed. upgrade refuses it, status says so, and install again finishes it.
echo "fail forum.install.sh" > "$sim/inject/install"
run 1 install --package "$pkg1" --port "$port"
absent "$sim/inject/install" "$root/etc/systemd/system/gono-forum-main.service" "$dir/config/gono-forum.toml"
binaries "" 0.1.0
run 1 upgrade --package "$pkg1"
says "gono-forum-main is not installed"
never_says "already runs"
run 0 status
says "Service    not installed"
pass "an install that stopped halfway is not taken for an installed instance"

run 0 install --package "$pkg1" --port "$port"
says "is installed as gono-forum-main"
binaries "" 0.1.0
serving 0.1.0
pass "install 0.1.0 over the install that stopped halfway"

mv "$dir/config/gono-forum.toml" "$sim/config.aside"
run 1 upgrade --package "$pkg2"
says "config/gono-forum.toml is missing"
binaries "" 0.1.0
serving 0.1.0
mv "$sim/config.aside" "$dir/config/gono-forum.toml"
pass "upgrade refuses an instance without its configuration"

run 0 upgrade --package "$pkg1"
says "already runs 0.1.0"
pass "upgrade to the installed version does nothing"

# (a) A step of the switch fails halfway: the web binary is already replaced when moving the CLI in fails.
echo "fail gono-forum-cli.new" > "$sim/inject/mv"
run 1 upgrade --package "$pkg2"
absent "$sim/inject/mv"
says "could not put the new binaries in place"
says "gono-forum-main runs 0.1.0 again"
binaries "" 0.1.0
absent "$dir/gono-forum-web.previous" "$dir/gono-forum-cli.previous"
no_leftovers
serving 0.1.0
pass "a failed rename halfway through the switch puts every binary back and restarts 0.1.0"

run 0 upgrade --package "$pkg2"
never_says "already runs"
says "upgraded from 0.1.0 to 0.2.0"
binaries "" 0.2.0
binaries .previous 0.1.0
no_leftovers
last_result upgraded
serving 0.2.0
pass "upgrade 0.1.0 -> 0.2.0 after the failed attempt"

run 0 rollback
binaries "" 0.1.0
binaries .previous 0.2.0
last_result switched
serving 0.1.0
run 0 rollback
binaries "" 0.2.0
binaries .previous 0.1.0
no_leftovers
serving 0.2.0
pass "rollback switches back and forward"

# (b) The new version accepts connections and never answers: the check gives up after its time limit.
echo hang > "$sim/behavior/0.3.0"
began=$SECONDS
run 1 upgrade --package "$pkg3"
took=$((SECONDS - began))
[ "$took" -ge 25 ] && [ "$took" -le 50 ] || fail "the hung version took ${took}s to roll back, expected about 30"
says "0.3.0 did not start or does not answer"
says "gono-forum-main runs 0.2.0 again"
binaries "" 0.2.0
binaries .previous 0.1.0
binaries .failed 0.3.0
no_leftovers
last_result rolled_back
serving 0.2.0
pass "a version that never answers is rolled back after ${took}s"

echo crash > "$sim/behavior/0.3.0"
run 1 upgrade --package "$pkg3"
says "gono-forum-main runs 0.2.0 again"
binaries "" 0.2.0
binaries .previous 0.1.0
no_leftovers
serving 0.2.0
pass "a version that exits before it is ready is rolled back"

# (b) The version put back does not start either: the script says so instead of claiming it runs again.
echo crash > "$sim/behavior/0.2.0"
run 1 upgrade --package "$pkg3"
says "but 0.2.0 does not start or answer either"
says "To do by hand:"
says "restore the"
never_says "runs 0.2.0 again"
binaries "" 0.2.0
binaries .previous 0.1.0
no_leftovers
last_result rolled_back
serving nothing
echo ok > "$sim/behavior/0.2.0"
systemctl start gono-forum-main
serving 0.2.0
pass "a failed restart after the rollback is reported, with what to do by hand"

# Interrupted while the new version is being checked: the exit handler puts the old binaries back.
echo hang > "$sim/behavior/0.3.0"
start_run upgrade --package "$pkg3"
for _ in $(seq 100); do
  [ "$("$dir/gono-forum-web" --version 2>/dev/null)" = "gono-forum-web 0.3.0" ] && [ -f "$sim/systemd/gono-forum-main.ready" ] && break
  sleep 0.1
done
sleep 2
kill -TERM "$(cat "$sim/script.pid")"
finish_run 143
says "stopped while switching the binaries"
binaries "" 0.2.0
binaries .previous 0.1.0
no_leftovers
serving 0.2.0
pass "SIGTERM during the health check puts 0.2.0 back"

# Killed outright halfway through the switch: the next upgrade finds switch-backup/ and puts it back first.
echo ok > "$sim/behavior/0.3.0"
echo "kill gono-forum-cli.new" > "$sim/inject/mv"
run 137 upgrade --package "$pkg3"
sleep 1
[ -d "$dir/switch-backup" ] || fail "switch-backup/ should be left behind"
reports "$dir/gono-forum-web" 0.3.0
reports "$dir/gono-forum-cli" 0.2.0
run 0 status
says "Unfinished"
says "but gono-forum-cli 0.2.0"
run 0 upgrade --package "$pkg3"
says "stopped while switching binaries; putting back the ones saved before it"
says "upgraded from 0.2.0 to 0.3.0"
binaries "" 0.3.0
binaries .previous 0.2.0
no_leftovers
serving 0.3.0
pass "a run killed halfway is undone by the next upgrade, which then goes ahead"

# A retry compares every binary: one binary of the new version is not a finished upgrade.
cp "$dir/gono-forum-cli.previous" "$dir/gono-forum-cli"
run 0 upgrade --package "$pkg3"
never_says "already runs"
says "upgraded from 0.3.0 to 0.3.0"
binaries "" 0.3.0
pass "a mixed install is upgraded again instead of reported as done"

run 1 rollback
says "the previous binaries are not one version"
binaries "" 0.3.0
no_leftovers
serving 0.3.0
pass "rollback refuses previous binaries of two versions"

# The start guard, after a one-click upgrade whose new version keeps failing.
echo crash > "$sim/behavior/0.4.0"
one_click 0.4.0
start_fails
start_fails
start_works
logged "switched back to the previous binaries"
binaries "" 0.3.0
binaries .failed 0.4.0
absent "$dir/gono-forum-web.previous" "$dir/gono-forum-cli.previous"
no_marker
no_leftovers
last_result rolled_back
grep -qx "to=0.4.0" "$dir/data/upgrade/last" || fail "data/upgrade/last does not name 0.4.0"
serving 0.3.0
pass "the start guard switches back on the third start"

one_click 0.4.0
start_fails
start_fails
echo "fail gono-forum-cli.previous" > "$sim/inject/mv"
start_fails
absent "$sim/inject/mv"
logged "the upgraded ones are in place again; not switching back automatically"
binaries "" 0.4.0
binaries .previous 0.3.0
no_marker
no_leftovers
serving nothing
run 0 rollback
serving 0.3.0
pass "a rename that fails in the start guard leaves the whole upgraded version and stops the automatic switch"

one_click 0.4.0
start_fails
start_fails
echo "cut gono-forum-cli.previous" > "$sim/inject/mv"
start_fails
[ -f "$dir/switch-backup/pending" ] || fail "the killed start guard should leave switch-backup/ with its marker"
reports "$dir/gono-forum-web" 0.3.0
reports "$dir/gono-forum-cli" 0.4.0
run 0 status
says "Unfinished"
start_works
logged "switched back to the previous binaries"
binaries "" 0.3.0
binaries .failed 0.4.0
no_marker
no_leftovers
serving 0.3.0
pass "a start guard killed halfway is finished by the next start"

one_click 0.4.0
rm "$dir/gono-forum-cli.previous"
start_fails
start_fails
start_fails
logged "gono-forum-cli.previous is missing; not switching back automatically"
binaries "" 0.4.0
reports "$dir/gono-forum-web.previous" 0.3.0
no_marker
no_leftovers
run 0 upgrade --package "$pkg3"
says "upgraded from 0.4.0 to 0.3.0"
serving 0.3.0
pass "the start guard changes nothing when a previous binary is missing"

one_click 0.4.0
mkdir "$dir/switch-backup"
cp "$dir/gono-forum-web.previous" "$dir/switch-backup/gono-forum-web"
cp "$dir/gono-forum-cli.previous" "$dir/switch-backup/gono-forum-cli"
start_fails
start_fails
start_fails
logged "switch-backup is left from an upgrade or rollback with forum.install.sh that did not finish"
binaries "" 0.4.0
[ -d "$dir/switch-backup" ] || fail "the start guard should leave a switch-backup/ that is not its own"
no_marker
run 0 upgrade --package "$pkg3"
says "putting back the ones saved before it"
says "already runs 0.3.0"
no_leftovers
serving 0.3.0
pass "the start guard leaves a switch-backup/ of forum.install.sh alone"

# A binary whose --version never returns.
echo hang > "$sim/behavior/0.5.0-cli-version"
began=$SECONDS
run 1 upgrade --package "$pkg5"
took=$((SECONDS - began))
[ "$took" -ge 14 ] && [ "$took" -le 30 ] || fail "the hung --version took ${took}s, expected about 15"
says "the packaged gono-forum-cli reports version (none) instead of 0.5.0; nothing was changed"
binaries "" 0.3.0
no_leftovers
serving 0.3.0
echo hang > "$sim/behavior/0.6.0-web-version"
began=$SECONDS
run 1 upgrade --package "$pkg6"
took=$((SECONDS - began))
[ "$took" -ge 14 ] && [ "$took" -le 30 ] || fail "the hung --version took ${took}s, expected about 15"
says "the packaged gono-forum-web does not run on this machine"
binaries "" 0.3.0
serving 0.3.0
pass "a --version that hangs is given up after 15s"

# The health check without curl: with wget (when it is there), then with bash's /dev/tcp; the listen address in single
# quotes with a comment after it. A version that answers with an error is rolled back, and the setup wizard's 503
# counts as an answer.
set_listen "listen = '127.0.0.1:$port'   # loopback only"
run 0 status
says "Listen     127.0.0.1:$port"
no_curl=$(without curl)
no_curl_or_wget=$(without curl wget)
if PATH=$base_path command -v wget >/dev/null; then
  echo error > "$sim/behavior/0.7.0"
  PATH=$no_curl run 1 upgrade --package "$pkg7"
  says "0.7.0 did not start or does not answer"
  says "gono-forum-main runs 0.3.0 again"
  binaries "" 0.3.0
  binaries .failed 0.7.0
  no_leftovers
  last_result rolled_back
  serving 0.3.0
  echo setup > "$sim/behavior/0.7.0"
  PATH=$no_curl run 0 upgrade --package "$pkg7"
  says "upgraded from 0.3.0 to 0.7.0"
  PATH=$no_curl run 0 rollback
  says "runs 0.3.0 again"
  serving 0.3.0
  pass "without curl, wget checks the new version"
else
  echo "skip without curl, wget checks the new version (no wget)"
fi
echo error > "$sim/behavior/0.7.0"
PATH=$no_curl_or_wget run 1 upgrade --package "$pkg7"
says "0.7.0 did not start or does not answer"
says "gono-forum-main runs 0.3.0 again"
binaries "" 0.3.0
no_leftovers
last_result rolled_back
serving 0.3.0
echo setup > "$sim/behavior/0.7.0"
PATH=$no_curl_or_wget run 0 upgrade --package "$pkg7"
says "upgraded from 0.3.0 to 0.7.0"
PATH=$no_curl_or_wget run 0 rollback
says "runs 0.3.0 again"
binaries "" 0.3.0
binaries .previous 0.7.0
serving 0.3.0
pass "without curl or wget, bash's /dev/tcp checks the new version"

# A listen address the script cannot read stops an upgrade before the service is stopped.
set_listen "listen = \"\"\"127.0.0.1:$port\"\"\""
run 0 status
says "Listen     unknown"
run 1 upgrade --package "$pkg7"
says "cannot read the listen address"
says "nothing was changed"
binaries "" 0.3.0
no_leftovers
serving 0.3.0
set_listen ""
run 0 status
says "Listen     127.0.0.1:58833"
set_listen "listen = \"127.0.0.1:$port\""
pass "an unreadable listen address stops the upgrade before anything changes; a missing one is the default"

# uninstall removes nothing while the service may still run.
echo "fail disable" > "$sim/inject/systemctl"
run 1 uninstall --purge --yes
absent "$sim/inject/systemctl"
says "could not stop and disable gono-forum-main"
says "nothing was removed"
[ -f "$root/etc/systemd/system/gono-forum-main.service" ] || fail "the unit should be kept"
binaries "" 0.3.0
serving 0.3.0
echo "ignore disable" > "$sim/inject/systemctl"
run 1 uninstall --purge --yes
says "is still running after systemctl disable --now"
says "nothing was removed"
[ -f "$dir/config/gono-forum.toml" ] || fail "the configuration should be kept"
serving 0.3.0
pass "uninstall stops when the service is not stopped"

run 0 uninstall
says "The configuration and data are kept"
serving nothing
run 1 install --package "$pkg3" --port "$((port + 1))"
says "is kept from an earlier install and sets the listen address 127.0.0.1:$port"
run 0 install --package "$pkg3" --port "$port"
says "is reinstalled as gono-forum-main"
serving 0.3.0
pass "uninstall keeps the configuration, and install with its listen address reinstalls"

touch "$sim/inject/userdel"
run 1 uninstall --purge --yes
says "could not delete the system account gono-forum-main"
never_says "with its configuration, data and account"
absent "$dir" "$root/usr/local/lib/gono-forum"
grep -qxF gono-forum-main "$sim/users" || fail "the fake account should still exist"
serving nothing
pass "uninstall --purge reports an account it could not delete"

echo "all simulated runs passed"
