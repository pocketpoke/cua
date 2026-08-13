#!/usr/bin/env bash
set -euo pipefail

CONFIG_FILE="${CUA_KWIN_HELPER_CONFIG:-kwinrc}"
CONFIG_GROUP="${CUA_KWIN_HELPER_GROUP:-Script-cua-kwin-helper}"
REQUEST_JSON_KEY=request_json
RESPONSE_JSON_KEY=response_json
CAPABILITIES_JSON_KEY=capabilities_json
SNAPSHOT_JSON_KEY=snapshot_json

usage() {
  cat <<'EOF'
Usage:
  probe-kwin.sh
  probe-kwin.sh --activate <window_id>

Without arguments this is read-only: it prints the helper capabilities,
snapshot, and most recent response from the helper config file.

With --activate it writes one activation request into the helper config,
verifies target focus, then restores the exact previously active window and
verifies restoration. This should only be run intentionally on the real
Plasma session.
EOF
}

read_key() {
  local key="$1"
  kreadconfig6 --file "${CONFIG_FILE}" --group "${CONFIG_GROUP}" --key "${key}" 2>/dev/null || true
}

write_key() {
  local key="$1"
  local value="$2"
  kwriteconfig6 --file "${CONFIG_FILE}" --group "${CONFIG_GROUP}" --key "${key}" "${value}"
  # KWin's legacy JavaScript API does not watch kwinrc for file changes.
  # Reconfigure emits options.configChanged, which is the helper's request
  # trigger. This D-Bus call is local control-plane traffic only.
  qdbus org.kde.KWin /KWin reconfigure >/dev/null
}

dump_state() {
  echo "Live script:"
  qdbus --literal org.kde.KWin /Scripting isScriptLoaded cua-kwin-helper 2>&1 || true
  echo "Publication bridge:"
  qdbus --literal org.cua.KWinHelper /org/cua/KWinHelper 2>&1 || true
  echo
  echo "Config: ${CONFIG_FILE}"
  echo "Group:  ${CONFIG_GROUP}"
  echo
  echo "[Capabilities]"
  read_key "${CAPABILITIES_JSON_KEY}"
  echo
  echo "[Snapshot]"
  read_key "${SNAPSHOT_JSON_KEY}"
  echo
  echo "[Response json]"
  read_key "${RESPONSE_JSON_KEY}"
}

handshake_previous_window() {
  local target_id="$1"
  local capabilities
  local snapshot
  capabilities="$(read_key "${CAPABILITIES_JSON_KEY}" | tr -d '\n')"
  snapshot="$(read_key "${SNAPSHOT_JSON_KEY}" | tr -d '\n')"
  CUA_CAPABILITIES_JSON="${capabilities}" \
  CUA_SNAPSHOT_JSON="${snapshot}" \
  CUA_TARGET_WINDOW_ID="${target_id}" \
    python3 - <<'PY'
import json
import os
import sys

try:
    capabilities = json.loads(os.environ["CUA_CAPABILITIES_JSON"])
    snapshot = json.loads(os.environ["CUA_SNAPSHOT_JSON"])
    target_id = int(os.environ["CUA_TARGET_WINDOW_ID"])
except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
    print(f"invalid KWin helper handshake JSON: {error}", file=sys.stderr)
    raise SystemExit(1)

if capabilities.get("version") != 2 or capabilities.get("compositor") != "kwin_wayland":
    print("KWin helper capability version/compositor mismatch", file=sys.stderr)
    raise SystemExit(1)
for key in ("can_snapshot", "can_activate_exact_window", "can_restore_focus"):
    if capabilities.get(key) is not True:
        print(f"KWin helper capability {key} is not true", file=sys.stderr)
        raise SystemExit(1)
if snapshot.get("version") != 2 or not isinstance(snapshot.get("windows"), list):
    print("KWin helper snapshot version/shape mismatch", file=sys.stderr)
    raise SystemExit(1)

windows = snapshot["windows"]
ids = [int(window["window_id"]) for window in windows]
if len(ids) != len(set(ids)):
    print("KWin helper snapshot contains duplicate window IDs", file=sys.stderr)
    raise SystemExit(1)
active = [window for window in windows if window.get("active") is True]
target = [window for window in windows if int(window.get("window_id", -1)) == target_id]
if len(active) != 1:
    print("KWin helper snapshot does not identify exactly one active window", file=sys.stderr)
    raise SystemExit(1)
if len(target) != 1 or int(target[0].get("pid", 0)) <= 0:
    print(f"KWin helper snapshot does not prove exact target {target_id}", file=sys.stderr)
    raise SystemExit(1)
if not target[0].get("visible") or target[0].get("minimized"):
    print(f"KWin helper target {target_id} is not visible and non-minimized", file=sys.stderr)
    raise SystemExit(1)
print(json.dumps({
    "previous_window_id": int(active[0]["window_id"]),
    "previous_pid": int(active[0]["pid"]),
    "target_pid": int(target[0]["pid"]),
}))
PY
}

wait_for_snapshot_focus() {
  local expected_id="$1"
  local expected_pid="$2"
  local deadline=$((SECONDS + 3))
  while (( SECONDS < deadline )); do
    local snapshot
    snapshot="$(read_key "${SNAPSHOT_JSON_KEY}" | tr -d '\n')"
    if CUA_SNAPSHOT_JSON="${snapshot}" CUA_EXPECTED_WINDOW_ID="${expected_id}" CUA_EXPECTED_PID="${expected_pid}" python3 - <<'PY'
import json
import os

try:
    snapshot = json.loads(os.environ["CUA_SNAPSHOT_JSON"])
    expected = int(os.environ["CUA_EXPECTED_WINDOW_ID"])
    expected_pid = int(os.environ["CUA_EXPECTED_PID"])
    active = [window for window in snapshot.get("windows", []) if window.get("active") is True]
    raise SystemExit(0 if len(active) == 1 and int(active[0]["window_id"]) == expected and int(active[0]["pid"]) == expected_pid else 1)
except (KeyError, TypeError, ValueError, json.JSONDecodeError):
    raise SystemExit(1)
PY
    then
      return 0
    fi
    sleep 0.05
  done
  echo "Timed out waiting for snapshot focus ${expected_id} / pid ${expected_pid}" >&2
  return 1
}

request_activation() {
  local window_id="$1"
  local pid="$2"
  local nonce
  nonce="$(printf '%s-%s-%s' "$(date +%s)" "$$" "$RANDOM")"
  local payload
  payload="$(printf '{"version":2,"nonce":"%s","action":"activate","pid":%s,"window_id":%s}' "${nonce}" "${pid}" "${window_id}")"

  write_key "${REQUEST_JSON_KEY}" "${payload}"

  local deadline=$((SECONDS + 3))
  while (( SECONDS < deadline )); do
    local response
    response="$(read_key "${RESPONSE_JSON_KEY}" | tr -d '\n')"
    if [[ -n "${response}" ]] && [[ "${response}" == *"\"nonce\":\"${nonce}\""* ]]; then
      printf '%s\n' "${response}"
      return 0
    fi
    sleep 0.05
  done

  echo "Timed out waiting for response nonce ${nonce}" >&2
  return 1
}

validate_response() {
  local response="$1"
  local expected_nonce="$2"
  local expected_active="$3"
  local expected_previous="$4"
  local expected_active_pid="$5"
  local expected_previous_pid="$6"
  CUA_RESPONSE_JSON="${response}" \
  CUA_EXPECTED_NONCE="${expected_nonce}" \
  CUA_EXPECTED_ACTIVE="${expected_active}" \
  CUA_EXPECTED_PREVIOUS="${expected_previous}" \
  CUA_EXPECTED_ACTIVE_PID="${expected_active_pid}" \
  CUA_EXPECTED_PREVIOUS_PID="${expected_previous_pid}" \
    python3 - <<'PY'
import json
import os
import sys

try:
    response = json.loads(os.environ["CUA_RESPONSE_JSON"])
except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
    print(f"invalid KWin helper response JSON: {error}", file=sys.stderr)
    raise SystemExit(1)
if str(response.get("nonce")) != os.environ["CUA_EXPECTED_NONCE"]:
    print("KWin helper response nonce mismatch", file=sys.stderr)
    raise SystemExit(1)
if response.get("ok") is not True:
    print(f"KWin helper rejected activation: {response.get('error', '')}", file=sys.stderr)
    raise SystemExit(1)
if int(response.get("active_window_id", -1)) != int(os.environ["CUA_EXPECTED_ACTIVE"]):
    print("KWin helper response active-window identity mismatch", file=sys.stderr)
    raise SystemExit(1)
if response.get("previous_window_id") != int(os.environ["CUA_EXPECTED_PREVIOUS"]):
    print("KWin helper response previous-window identity mismatch", file=sys.stderr)
    raise SystemExit(1)
if int(response.get("active_pid", -1)) != int(os.environ["CUA_EXPECTED_ACTIVE_PID"]):
    print("KWin helper response active-pid identity mismatch", file=sys.stderr)
    raise SystemExit(1)
if int(response.get("previous_pid", -1)) != int(os.environ["CUA_EXPECTED_PREVIOUS_PID"]):
    print("KWin helper response previous-pid identity mismatch", file=sys.stderr)
    raise SystemExit(1)
PY
}

activate() {
  local window_id="$1"
  local handshake
  handshake="$(handshake_previous_window "${window_id}")" || return 1
  local previous_window_id target_pid previous_pid
  previous_window_id="$(printf '%s' "${handshake}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["previous_window_id"])')"
  previous_pid="$(printf '%s' "${handshake}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["previous_pid"])')"
  target_pid="$(printf '%s' "${handshake}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_pid"])')"

  local response=""
  local activation_rc=0
  response="$(request_activation "${window_id}" "${target_pid}")" || activation_rc=$?
  if [[ "${activation_rc}" -eq 0 ]]; then
    local nonce
    nonce="$(printf '%s\n' "${response}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["nonce"])')" || activation_rc=1
    if [[ "${activation_rc}" -eq 0 ]]; then
      validate_response "${response}" "${nonce}" "${window_id}" "${previous_window_id}" "${target_pid}" "${previous_pid}" || activation_rc=1
      wait_for_snapshot_focus "${window_id}" "${target_pid}" || activation_rc=1
    fi
  fi

  local restore_response=""
  local restore_rc=0
  restore_response="$(request_activation "${previous_window_id}" "${previous_pid}")" || restore_rc=$?
  if [[ "${restore_rc}" -eq 0 ]]; then
    local restore_nonce
    restore_nonce="$(printf '%s\n' "${restore_response}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["nonce"])')" || restore_rc=1
    if [[ "${restore_rc}" -eq 0 ]]; then
      validate_response "${restore_response}" "${restore_nonce}" "${previous_window_id}" "${window_id}" "${previous_pid}" "${target_pid}" || restore_rc=1
      wait_for_snapshot_focus "${previous_window_id}" "${previous_pid}" || restore_rc=1
    fi
  fi

  printf '[Activation response]\n%s\n[Restore response]\n%s\n' "${response}" "${restore_response}"
  if [[ "${activation_rc}" -ne 0 ]]; then
    echo "Exact target activation verification failed" >&2
    return "${activation_rc}"
  fi
  if [[ "${restore_rc}" -ne 0 ]]; then
    echo "Exact focus restoration verification failed" >&2
    return "${restore_rc}"
  fi
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ $# -eq 0 ]]; then
  dump_state
  exit 0
fi

if [[ $# -eq 2 && "${1}" == "--activate" ]]; then
  activate "${2}"
  exit 0
fi

usage >&2
exit 1
