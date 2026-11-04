#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LLAMA_DIR="${DORY_LLAMA_DIR:-}"
if [[ -z "$LLAMA_DIR" || ! -d "$LLAMA_DIR/build/bin" ]]; then
  echo "Set DORY_LLAMA_DIR to the pinned llama.cpp-dory checkout with build/bin libraries." >&2
  exit 2
fi
cd "$ROOT"
cargo build --release -p dory-gui
stage="$ROOT/dist/dory-harness-linux-x86_64"
rm -rf "$stage"
mkdir -p "$stage/lib"
cp target/release/dory-gui "$stage/dory-gui.bin"
for pattern in libllama.so* libggml*.so*; do
  compgen -G "$LLAMA_DIR/build/bin/$pattern" >/dev/null || continue
  cp -a "$LLAMA_DIR"/build/bin/$pattern "$stage/lib/"
done
cat > "$stage/dory-gui" <<'LAUNCHER'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/dory-gui.bin" "$@"
LAUNCHER
chmod +x "$stage/dory-gui"
tar -C "$ROOT/dist" -czf "$ROOT/dist/dory-harness-linux-x86_64.tar.gz" dory-harness-linux-x86_64
echo "Created $ROOT/dist/dory-harness-linux-x86_64.tar.gz"
