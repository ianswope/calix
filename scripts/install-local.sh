#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID="com.ianswope.Calix"

if [[ "${CALIX_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml"
fi

install -Dm755 "$ROOT_DIR/target/release/calix" "$BIN_DIR/calix"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.desktop" "$DATA_DIR/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.svg" "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
fi

cat <<EOF
Calix installed.

Binary: $BIN_DIR/calix
Desktop entry: $DATA_DIR/applications/$APP_ID.desktop

Make sure $BIN_DIR is on PATH if you want to run 'calix' from a terminal.
EOF
