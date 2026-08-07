#!/usr/bin/env bash
# Install AnythingUse Chrome control (Native Messaging host + unpacked
# extension) into the Runtime private entry, independent of the repo tree.
#
# The host script and the extension are COPIED under RUNTIME_ROOT so the
# Chrome surface keeps working when the project directory moves or is
# deleted. The extension ID is manifest-key-derived, so copying never
# changes it. Re-running this script overwrites in place; Chrome
# auto-reloads the unpacked extension on file changes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST_SRC="$ROOT/native-host/host.mjs"
TEMPLATE="$ROOT/native-host/com.lcu.chrome_control.json.template"
HOST_NAME="com.lcu.chrome_control"
EXT_ID="${LCU_CHROME_EXT_ID:-dcmfagbbcdpbmggmpngkhegbidepcogk}"

[[ -f "$HOST_SRC" ]] || { echo "missing host: $HOST_SRC" >&2; exit 1; }
[[ -f "$TEMPLATE" ]] || { echo "missing template: $TEMPLATE" >&2; exit 1; }
[[ -d "$ROOT/extension" ]] || { echo "missing extension: $ROOT/extension" >&2; exit 1; }

chmod +x "$HOST_SRC" || true

resolve_node() {
  local cand real
  for cand in \
    "$(command -v node 2>/dev/null || true)" \
    "${HOME}/.local/share/fnm/aliases/default/bin/node" \
    /opt/homebrew/bin/node \
    /usr/local/bin/node
  do
    [[ -n "$cand" && -x "$cand" ]] || continue
    real="$(python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "$cand" 2>/dev/null || echo "$cand")"
    [[ "$real" == *fnm_multishells* ]] && continue
    if [[ -x "$real" ]]; then
      echo "$real"
      return 0
    fi
  done
  cand="$(command -v node 2>/dev/null || true)"
  if [[ -n "$cand" && -x "$cand" ]]; then
    python3 -c "import os,sys; print(os.path.realpath(sys.argv[1]))" "$cand"
    return 0
  fi
  return 1
}

NODE_BIN="$(resolve_node || true)"
[[ -n "$NODE_BIN" && -x "$NODE_BIN" ]] || { echo "node not found on PATH" >&2; exit 1; }

# Runtime private entry layout (matches lcu-runtime RuntimePaths).
RUNTIME_ROOT="${LCU_RUNTIME_ROOT:-${HOME}/Library/Application Support/AnythingUse}"
mkdir -p "$RUNTIME_ROOT/logs"
chmod 700 "$RUNTIME_ROOT" 2>/dev/null || true

# Copy host + extension out of the repo tree (overwrite in place on re-run).
INSTALL_HOST="$RUNTIME_ROOT/native-host/host.mjs"
INSTALL_EXT="$RUNTIME_ROOT/chrome-extension"
mkdir -p "$(dirname "$INSTALL_HOST")" "$INSTALL_EXT"
cp "$HOST_SRC" "$INSTALL_HOST"
cp -R "$ROOT/extension/." "$INSTALL_EXT/"
chmod -R u+rX "$INSTALL_EXT"

WRAPPER="$RUNTIME_ROOT/chrome-control-host-wrapper.sh"
# Quote path safely for bash 3.2 (macOS /bin/bash) without ${var@Q}.
RUNTIME_ROOT_Q=$(python3 -c "import shlex,sys; print(shlex.quote(sys.argv[1]))" "$RUNTIME_ROOT")
cat >"$WRAPPER" <<EOF
#!/bin/bash
export LCU_RUNTIME_ROOT=$RUNTIME_ROOT_Q
exec "$NODE_BIN" "$INSTALL_HOST" "\$@"
EOF
chmod +x "$WRAPPER"

NM_DIR="${HOME}/Library/Application Support/Google/Chrome/NativeMessagingHosts"
mkdir -p "$NM_DIR"
TARGET="$NM_DIR/${HOST_NAME}.json"

python3 -c "
import json
data=json.load(open('''$TEMPLATE'''))
data['path']='''$WRAPPER'''
data['name']='$HOST_NAME'
data['allowed_origins']=['chrome-extension://$EXT_ID/']
json.dump(data, open('''$TARGET''','w'), indent=2)
"

echo "Installed AnythingUse Chrome control:"
echo "  name:         $HOST_NAME"
echo "  host wrapper: $WRAPPER"
echo "  node:         $NODE_BIN"
echo "  host:         $INSTALL_HOST (copied; repo-independent)"
echo "  extension:    $INSTALL_EXT  (copied; repo-independent)"
echo "  manifest:     $TARGET"
echo "  ext id:       $EXT_ID"
echo "  runtime root: $RUNTIME_ROOT"
echo "  control sock: $RUNTIME_ROOT/chrome-control.sock  (unix only, no TCP)"
echo

CHROMIUM_DIR="${HOME}/Library/Application Support/Chromium/NativeMessagingHosts"
if [[ -d "${HOME}/Library/Application Support/Chromium" ]]; then
  mkdir -p "$CHROMIUM_DIR"
  cp "$TARGET" "$CHROMIUM_DIR/${HOST_NAME}.json"
  echo "  chromium:     $CHROMIUM_DIR/${HOST_NAME}.json"
fi

echo
echo "Next:"
echo "  1. Chrome → chrome://extensions → Developer mode → Load unpacked:"
echo "       $INSTALL_EXT"
echo "     (Cmd+Shift+G in the dialog; ~/Library is hidden by default)"
echo "  2. Confirm extension ID is $EXT_ID (manifest key fixes this)."
echo "  3. Control socket appears at: \$LCU_RUNTIME_ROOT/chrome-control.sock when extension connects."
echo
echo "Update: re-run this script — copied host and extension are overwritten"
echo "in place; Chrome auto-reloads the unpacked extension on file changes."
