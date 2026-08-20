#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID="com.ianswope.Calix"

rm -f "$BIN_DIR/calix"
rm -f "$DATA_DIR/applications/$APP_ID.desktop"
rm -f "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"
rm -f "$DATA_DIR/metainfo/$APP_ID.metainfo.xml"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
fi

cat <<EOF
Calix uninstalled from user-local application locations.

Your calendars, settings, and saved account credentials were kept:
  Data:   ${XDG_DATA_HOME:-$HOME/.local/share}/calix
  Config: ${XDG_CONFIG_HOME:-$HOME/.config}/calix

Remove accounts inside Calix before uninstalling if you also want their saved
credentials removed from the system keyring.
EOF
