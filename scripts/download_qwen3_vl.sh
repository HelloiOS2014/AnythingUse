#!/usr/bin/env bash
# Download Qwen3-VL-4B-Instruct weights for the local AnythingUse / lcu VLM actor.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${LCU_MODEL_DIR:-$ROOT/models/Qwen3-VL-4B-Instruct}"
REPO="${LCU_MODEL_REPO:-Qwen/Qwen3-VL-4B-Instruct}"

mkdir -p "$OUT"
echo "target dir: $OUT"
echo "repo: $REPO"

if command -v hf >/dev/null 2>&1; then
  hf download "$REPO" --local-dir "$OUT"
elif python3 -c 'import huggingface_hub' 2>/dev/null; then
  python3 - <<PY
from huggingface_hub import snapshot_download
print(snapshot_download(repo_id="$REPO", local_dir="$OUT"))
PY
elif [[ -x "$ROOT/.venv/bin/python" ]] && "$ROOT/.venv/bin/python" -c 'import huggingface_hub' 2>/dev/null; then
  "$ROOT/.venv/bin/python" - <<PY
from huggingface_hub import snapshot_download
print(snapshot_download(repo_id="$REPO", local_dir="$OUT"))
PY
else
  echo "ERROR: need \`hf\` CLI or python huggingface_hub" >&2
  echo "  pip install -U huggingface_hub" >&2
  echo "  # or: brew install huggingface-cli / install hf" >&2
  exit 1
fi

SIZE=$(du -sk "$OUT" | awk '{print $1 * 1024}')
echo "{\"repo\":\"$REPO\",\"local_dir\":\"$OUT\",\"bytes\":$SIZE}" | tee "$OUT/lcu-download-manifest.json"
echo "download complete ($SIZE bytes)"
