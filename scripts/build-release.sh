#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.ianswope.Calix"
VERSION="$(grep -m1 '^version =' "$ROOT_DIR/Cargo.toml" | cut -d '"' -f2)"
ARCH="$(uname -m)"
PACKAGE="calix-$VERSION-linux-$ARCH"
STAGE="$ROOT_DIR/target/dist/$PACKAGE"

cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml"

rm -rf "$STAGE"
install -Dm755 "$ROOT_DIR/target/release/calix" "$STAGE/bin/calix"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.svg" "$STAGE/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT_DIR/README.md" "$STAGE/share/doc/calix/README.md"
install -Dm644 "$ROOT_DIR/LICENSE" "$STAGE/share/doc/calix/LICENSE"

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
APP_ID="com.ianswope.Calix"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

install -Dm755 "$ROOT_DIR/bin/calix" "$PREFIX/bin/calix"
install -Dm644 "$ROOT_DIR/share/applications/$APP_ID.desktop" "$PREFIX/share/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/share/icons/hicolor/scalable/apps/$APP_ID.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT_DIR/share/doc/calix/README.md" "$PREFIX/share/doc/calix/README.md"
install -Dm644 "$ROOT_DIR/share/doc/calix/LICENSE" "$PREFIX/share/doc/calix/LICENSE"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Calix installed to $PREFIX"
INSTALL
chmod +x "$STAGE/install.sh"

tar -C "$ROOT_DIR/target/dist" -czf "$ROOT_DIR/target/dist/$PACKAGE.tar.gz" "$PACKAGE"

echo "Built $ROOT_DIR/target/dist/$PACKAGE.tar.gz"
