#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.ianswope.Calix"
VERSION="$(grep -m1 '^version =' "$ROOT_DIR/Cargo.toml" | cut -d '"' -f2)"
ARCH="$(uname -m)"
PACKAGE="calix-$VERSION-linux-$ARCH"
STAGE="$ROOT_DIR/target/dist/$PACKAGE"

if [[ "${CALIX_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml"
fi

if [[ ! -x "$ROOT_DIR/target/release/calix" ]]; then
  echo "error: target/release/calix is missing; build it first or unset CALIX_SKIP_BUILD" >&2
  exit 1
fi

rm -rf "$STAGE"
install -Dm755 "$ROOT_DIR/target/release/calix" "$STAGE/bin/calix"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.svg" "$STAGE/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.metainfo.xml" "$STAGE/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 "$ROOT_DIR/README.md" "$STAGE/share/doc/calix/README.md"
install -Dm644 "$ROOT_DIR/CHANGELOG.md" "$STAGE/share/doc/calix/CHANGELOG.md"
install -Dm644 "$ROOT_DIR/LICENSE" "$STAGE/share/doc/calix/LICENSE"

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
APP_ID="com.ianswope.Calix"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

install -Dm755 "$ROOT_DIR/bin/calix" "$PREFIX/bin/calix"
install -Dm644 "$ROOT_DIR/share/applications/$APP_ID.desktop" "$PREFIX/share/applications/$APP_ID.desktop"
# Desktop sessions do not reliably include ~/.local/bin in PATH. Use the exact
# binary installed by this archive so launching from an app menu always works.
sed -i "s|^Exec=calix$|Exec=$PREFIX/bin/calix|" "$PREFIX/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/share/icons/hicolor/scalable/apps/$APP_ID.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT_DIR/share/metainfo/$APP_ID.metainfo.xml" "$PREFIX/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 "$ROOT_DIR/share/doc/calix/README.md" "$PREFIX/share/doc/calix/README.md"
install -Dm644 "$ROOT_DIR/share/doc/calix/CHANGELOG.md" "$PREFIX/share/doc/calix/CHANGELOG.md"
install -Dm644 "$ROOT_DIR/share/doc/calix/LICENSE" "$PREFIX/share/doc/calix/LICENSE"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

cat <<EOF
Calix installed to $PREFIX

Re-run install.sh from a newer release archive to update.
Run uninstall.sh from this archive to remove the application. Your calendars,
settings, and saved account credentials are kept by default.
EOF
INSTALL
chmod +x "$STAGE/install.sh"

cat > "$STAGE/uninstall.sh" <<'UNINSTALL'
#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
APP_ID="com.ianswope.Calix"

rm -f "$PREFIX/bin/calix"
rm -f "$PREFIX/share/applications/$APP_ID.desktop"
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
rm -f "$PREFIX/share/metainfo/$APP_ID.metainfo.xml"
rm -f "$PREFIX/share/doc/calix/README.md"
rm -f "$PREFIX/share/doc/calix/CHANGELOG.md"
rm -f "$PREFIX/share/doc/calix/LICENSE"
rmdir "$PREFIX/share/doc/calix" 2>/dev/null || true

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

cat <<EOF
Calix was removed from $PREFIX.

Your calendars, settings, and saved account credentials were kept:
  Data:   ${XDG_DATA_HOME:-$HOME/.local/share}/calix
  Config: ${XDG_CONFIG_HOME:-$HOME/.config}/calix

Remove accounts inside Calix before uninstalling if you also want their saved
credentials removed from the system keyring.
EOF
UNINSTALL
chmod +x "$STAGE/uninstall.sh"

tar -C "$ROOT_DIR/target/dist" -czf "$ROOT_DIR/target/dist/$PACKAGE.tar.gz" "$PACKAGE"

echo "Built $ROOT_DIR/target/dist/$PACKAGE.tar.gz"
