#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LLAMA_DIR="${RILLWRIGHT_LLAMA_DIR:-${DORY_LLAMA_DIR:-}}"
if [[ -z "$LLAMA_DIR" || ! -d "$LLAMA_DIR/build/bin" ]]; then
  echo "Set RILLWRIGHT_LLAMA_DIR (legacy DORY_LLAMA_DIR) to the pinned llama.cpp-dory checkout." >&2
  exit 2
fi
cd "$ROOT"
cargo build --release -p rillwright-gui
stage="$ROOT/dist/rillwright-linux-x86_64"
rm -rf "$stage"
mkdir -p "$stage/lib" "$stage/share/icons/hicolor/512x512/apps"
cp target/release/rillwright-gui "$stage/rillwright-gui.bin"
cp crates/rillwright-gui/assets/rillwright.png "$stage/share/icons/hicolor/512x512/apps/rillwright.png"
for pattern in libllama.so* libggml*.so*; do
  compgen -G "$LLAMA_DIR/build/bin/$pattern" >/dev/null || continue
  cp -a "$LLAMA_DIR"/build/bin/$pattern "$stage/lib/"
done
cat > "$stage/rillwright-gui" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/rillwright-gui.bin" "$@"
LAUNCHER
chmod +x "$stage/rillwright-gui"
cat > "$stage/dory-gui" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
exec "$HERE/rillwright-gui" "$@"
LAUNCHER
chmod +x "$stage/dory-gui"
tar -C "$ROOT/dist" -czf "$ROOT/dist/rillwright-linux-x86_64.tar.gz" rillwright-linux-x86_64
echo "Created $ROOT/dist/rillwright-linux-x86_64.tar.gz"
