#!/usr/bin/env bash
set -euo pipefail

HOST_NAME="com.lcu.chrome_control"
RUNTIME_ROOT="${LCU_RUNTIME_ROOT:-${HOME}/Library/Application Support/AnythingUse}"

rm -f "${HOME}/Library/Application Support/Google/Chrome/NativeMessagingHosts/${HOST_NAME}.json"
rm -f "${HOME}/Library/Application Support/Chromium/NativeMessagingHosts/${HOST_NAME}.json"
rm -f "${RUNTIME_ROOT}/chrome-control.sock"
rm -f "${RUNTIME_ROOT}/chrome-control-host-wrapper.sh"

# Legacy spike paths (safe cleanup if present).
rm -f "${HOME}/.lcu-chrome-spike/control.sock"
rm -f "${HOME}/.lcu-chrome-spike/host-wrapper.sh"
rm -f "${HOME}/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.lcu.chrome_spike.json"

echo "Removed native messaging host manifests for $HOST_NAME"
echo "Private control socket path: ${RUNTIME_ROOT}/chrome-control.sock"
