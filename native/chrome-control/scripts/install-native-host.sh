#!/usr/bin/env bash
# Install Native Messaging host manifest for Google Chrome (user-level).
# Control socket is created under Runtime private entry (LocalComputerUse).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST_SRC="$ROOT/native-host/host.mjs"
TEMPLATE="$ROOT/native-host/com.lcu.chrome_control.json.template"
HOST_NAME="com.lcu.chrome_control"
EXT_ID="${LCU_CHROME_EXT_ID:-glcqmejd6hdgz7lkorqygtzonjxogz4b}"

if [[ ! -f "$HOST_SRC" ]]; then
  echo "missing host: $HOST_SRC" >&2
  exit 1
fi

if [[ ! -f "$TEMPLATE" ]]; then
  echo "missing template: $TEMPLATE" >&2
  exit 1
fi

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
    if [[ "$real" == *fnm_multishells* ]]; then
      continue
    fi
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
if [[ -z "$NODE_BIN" || ! -x "$NODE_BIN" ]]; then
  echo "node not found on PATH" >&2
  exit 1
fi

# Runtime private entry layout (matches lcu-runtime RuntimePaths).
RUNTIME_ROOT="${LCU_RUNTIME_ROOT:-${HOME}/Library/Application Support/LocalComputerUse}"
mkdir -p "$RUNTIME_ROOT/logs"
chmod 700 "$RUNTIME_ROOT" 2>/dev/null || true

WRAP_DIR="$RUNTIME_ROOT"
WRAPPER="$WRAP_DIR/chrome-control-host-wrapper.sh"
# Quote path safely for bash 3.2 (macOS /bin/bash) without ${var@Q}.
RUNTIME_ROOT_Q=$(python3 -c "import shlex,sys; print(shlex.quote(sys.argv[1]))" "$RUNTIME_ROOT")
cat >"$WRAPPER" <<EOF
#!/bin/bash
export LCU_RUNTIME_ROOT=$RUNTIME_ROOT_Q
exec "$NODE_BIN" "$HOST_SRC" "\$@"
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
print()
"

echo "Installed native messaging host:"
echo "  name:         $HOST_NAME"
echo "  host wrapper: $WRAPPER"
echo "  node:         $NODE_BIN"
echo "  script:       $HOST_SRC"
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
echo "       $ROOT/extension"
echo "  2. Confirm extension ID is $EXT_ID (manifest key fixes this)."
echo "  3. Control socket appears at: \$LCU_RUNTIME_ROOT/chrome-control.sock when extension connects."
