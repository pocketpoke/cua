#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PKG="${ROOT}/kwin-cua-helper"
ID="cua-kwin-helper"
ENABLE_KEY="${ID}Enabled"
CONFIG_FILE="${CUA_KWIN_HELPER_CONFIG:-kwinrc}"
CONFIG_GROUP="${CUA_KWIN_HELPER_GROUP:-Script-cua-kwin-helper}"
BRIDGE_BIN="${CUA_KWIN_HELPER_BRIDGE:-${ROOT}/../rust/target/release/kwin-helper-bridge}"

if [[ "${CUA_KWIN_HELPER_EXPERIMENTAL:-0}" != "1" ]]; then
  echo "KWin helper prototype is isolated and disabled by default; no production KDE support is claimed." >&2
  echo "Set CUA_KWIN_HELPER_EXPERIMENTAL=1 only for an intentional local prototype test." >&2
  exit 2
fi

if [[ ! -x "${BRIDGE_BIN}" ]]; then
  echo "A built kwin-helper-bridge is required before installing the prototype: ${BRIDGE_BIN}" >&2
  echo "Build it with the opt-in kwin-helper-bridge feature in the wayland-helper README." >&2
  exit 1
fi

if ! command -v kpackagetool6 >/dev/null; then
  echo "kpackagetool6 is required to install the KWin helper prototype" >&2
  exit 1
fi

echo "Installing KWin script package from: ${PKG}"
kpackagetool6 --type KWin/Script --upgrade "${PKG}" 2>/dev/null || \
  kpackagetool6 --type KWin/Script --install "${PKG}"

cat <<EOF

The package is installed, but enabling/reloading it changes the live desktop.
Start the local publication bridge in the real KDE session and leave it running:

  "${BRIDGE_BIN}"

Then load this exact declarative entry point through KWin's Plasma 6 D-Bus API:

  qdbus --literal org.kde.KWin /Scripting loadDeclarativeScript "${PKG}/contents/ui/main.qml" ${ID}
  qdbus --literal org.kde.KWin /Scripting isScriptLoaded ${ID}

If the direct load reports a path or API error, do not enable the plugin; fix
the reported error first. The alternative package/plugin path is:

  kwriteconfig6 --file kwinrc --group Plugins --key ${ENABLE_KEY} true
  qdbus org.kde.KWin /KWin reconfigure

Then verify the helper state file exists and carries capabilities/snapshot data:

  ${ROOT}/probe-kwin.sh

The probe's --activate mode is an intentional focus-changing test. Do not use
it as a capability claim unless both target activation and exact restoration
are observed successfully.

The helper publishes its control plane through KWin's own script config:

  ${CONFIG_FILE} [${CONFIG_GROUP}]

Rollback commands:

  qdbus --literal org.kde.KWin /Scripting unloadScript ${ID}
  kwriteconfig6 --file kwinrc --group Plugins --key ${ENABLE_KEY} false
  dbus-send --session --type=method_call --dest=org.kde.KWin /KWin org.kde.KWin.reconfigure
  kpackagetool6 --type KWin/Script --remove ${ID}
EOF
