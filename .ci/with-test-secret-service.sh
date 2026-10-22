#!/usr/bin/env bash
set -euo pipefail
# Isolate both the daemon and every XDG keyring path from the login session.
test "$#" -gt 0
scratch_home="$(mktemp -d)"
trap 'rm -rf -- "$scratch_home"' EXIT
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export HOME="$scratch_home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$HOME/.runtime"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export GNOME_KEYRING_CONTROL="$XDG_RUNTIME_DIR/keyring"
mkdir -p "$GNOME_KEYRING_CONTROL"
chmod 700 "$GNOME_KEYRING_CONTROL"
unset DISPLAY WAYLAND_DISPLAY
# Expand the quoted script inside the isolated D-Bus session, not its caller.
# shellcheck disable=SC2016
dbus-run-session -- bash -euo pipefail -c '
  daemon_pid=
  cleanup_daemon() {
    if [ -n "$daemon_pid" ]; then
      kill "$daemon_pid" 2>/dev/null || true
      wait "$daemon_pid" 2>/dev/null || true
    fi
  }
  trap cleanup_daemon EXIT
  # Login reads the password before initialization. The later --start connects
  # to this exact owned socket, rather than selecting a second locked daemon.
  printf "%s" "savfox-test-keyring" |
    gnome-keyring-daemon --foreground --login --components=secrets \
      --control-directory="$GNOME_KEYRING_CONTROL" >"$HOME/keyring-daemon.log" 2>&1 &
  daemon_pid=$!
  ready=false
  for ((attempt = 0; attempt < 100; attempt++)); do
    kill -0 "$daemon_pid" 2>/dev/null
    if test -S "$GNOME_KEYRING_CONTROL/control"; then
      ready=true
      break
    fi
    sleep 0.1
  done
  if [ "$ready" != true ]; then
    echo "Owned Secret Service did not become ready" >&2
    exit 1
  fi
  gnome-keyring-daemon --start --components=secrets \
    --control-directory="$GNOME_KEYRING_CONTROL" >"$HOME/keyring-start.log" 2>&1
  kill -0 "$daemon_pid" 2>/dev/null
  dbus-send --session --print-reply --dest=org.freedesktop.DBus \
    /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner \
    string:org.freedesktop.secrets | grep -q "boolean true"
  printf "%s" "durable" | secret-tool store --label=arkret-test-preflight service savfox.test.preflight account preflight
  test "$(secret-tool lookup service savfox.test.preflight account preflight)" = durable
  secret-tool clear service savfox.test.preflight account preflight
  "$@"
' bash "$@"
