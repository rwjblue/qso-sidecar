#!/usr/bin/env bash

set -euo pipefail

binary="$1"
port="$2"
expected_version="$3"
sidecar_pid=""
smoke_directory="$(mktemp -d)"

stop_sidecar() {
  if [[ -n "$sidecar_pid" ]] && kill -0 "$sidecar_pid" 2>/dev/null; then
    kill -INT "$sidecar_pid"
    wait "$sidecar_pid"
  fi
  sidecar_pid=""
}
trap 'stop_sidecar; rm -rf "$smoke_directory"' EXIT

run_once() {
  local run_port="$1"
  "$binary" --demo --no-rbn --port "$run_port" \
    --lofi-base http://127.0.0.1:9 >"${RUNNER_TEMP:-/tmp}/qso-sidecar-smoke.log" 2>&1 &
  sidecar_pid="$!"

  local ready=false
  for _attempt in {1..40}; do
    if curl --fail --silent "http://127.0.0.1:${run_port}/healthz" >"$smoke_directory/health.json"; then
      ready=true
      break
    fi
    sleep 0.25
  done
  if [[ "$ready" != true ]]; then
    echo "QSO Sidecar did not become healthy" >&2
    return 1
  fi

  curl --fail --silent "http://127.0.0.1:${run_port}/" | grep --quiet "QSO Sidecar"
  curl --fail --silent "http://127.0.0.1:${run_port}/app.js" | grep --quiet "EventSource"
  curl --fail --silent "http://127.0.0.1:${run_port}/api/state" >"$smoke_directory/state.json"
  python3 - "$expected_version" "$smoke_directory" <<'PY'
import json
import pathlib
import sys

directory = pathlib.Path(sys.argv[2])
with (directory / "health.json").open(encoding="utf-8") as stream:
    health = json.load(stream)
with (directory / "state.json").open(encoding="utf-8") as stream:
    state = json.load(stream)
assert health == {"ok": True, "version": sys.argv[1]}, health
assert state["demo"] is True, state["demo"]
assert state["spots_enabled"] is False, state["spots_enabled"]
PY

  stop_sidecar
}

"$binary" --version
"$binary" --help >/dev/null
run_once "$port"
run_once "$((port + 1))"
