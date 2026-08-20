#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.ianswope.Calix"
VERSION="$(grep -m1 '^version =' "$ROOT_DIR/Cargo.toml" | cut -d '"' -f2)"
PACKAGE="calix-$VERSION-linux-$(uname -m)"
ARCHIVE="$ROOT_DIR/target/dist/$PACKAGE.tar.gz"
CHECK_DIR="$(mktemp -d)"
trap 'rm -rf "$CHECK_DIR"' EXIT

"$ROOT_DIR/scripts/build-release.sh"
tar -C "$CHECK_DIR" -xzf "$ARCHIVE"

STAGE="$CHECK_DIR/$PACKAGE"
PREFIX="$CHECK_DIR/prefix"
PREFIX="$PREFIX" "$STAGE/install.sh"

test -x "$PREFIX/bin/calix"
test -x "$STAGE/install.sh"
test -x "$STAGE/uninstall.sh"
test -f "$PREFIX/share/applications/$APP_ID.desktop"
test -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
test -f "$PREFIX/share/metainfo/$APP_ID.metainfo.xml"
test -f "$PREFIX/share/doc/calix/README.md"
test -f "$PREFIX/share/doc/calix/LICENSE"
grep -Fqx "Exec=$PREFIX/bin/calix" "$PREFIX/share/applications/$APP_ID.desktop"
desktop-file-validate "$PREFIX/share/applications/$APP_ID.desktop"
appstreamcli validate --no-net "$PREFIX/share/metainfo/$APP_ID.metainfo.xml"
"$PREFIX/bin/calix" --version

PREFIX="$PREFIX" "$STAGE/uninstall.sh"
test ! -e "$PREFIX/bin/calix"
test ! -e "$PREFIX/share/applications/$APP_ID.desktop"
test ! -e "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
test ! -e "$PREFIX/share/metainfo/$APP_ID.metainfo.xml"
test ! -e "$PREFIX/share/doc/calix"

echo "Release archive install/uninstall check passed."
