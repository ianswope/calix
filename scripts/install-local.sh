#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID="com.ianswope.Calix"

# The binary carries the commit it was built from (see build.rs), which is only
# worth trusting if the tree was clean. Refuse a dirty install rather than stamp
# a commit hash onto a build that isn't that commit.
if git -C "$ROOT_DIR" rev-parse --git-dir >/dev/null 2>&1; then
  if [[ -n "$(git -C "$ROOT_DIR" status --porcelain)" ]]; then
    if [[ "${CALIX_ALLOW_DIRTY:-0}" == "1" ]]; then
      echo "warning: installing from a dirty tree (CALIX_ALLOW_DIRTY=1)." >&2
      echo "         The stamped commit will not match what you installed." >&2
    else
      echo "error: uncommitted changes — the installed build would be stamped" >&2
      echo "       with a commit it isn't. Commit them, or set" >&2
      echo "       CALIX_ALLOW_DIRTY=1 to install anyway." >&2
      echo >&2
      git -C "$ROOT_DIR" status --short >&2
      exit 1
    fi
  fi

  # Informational only: the local upstream ref may itself be stale, and being
  # ahead of origin is normal mid-work. Not a reason to block an install.
  if upstream="$(git -C "$ROOT_DIR" rev-parse --abbrev-ref '@{upstream}' 2>/dev/null)"; then
    behind="$(git -C "$ROOT_DIR" rev-list --count "HEAD..$upstream" 2>/dev/null || echo 0)"
    if [[ "$behind" -gt 0 ]]; then
      echo "note: HEAD is $behind commit(s) behind $upstream; 'git pull' first" >&2
      echo "      if you meant to install the latest." >&2
    fi
  fi
fi

if [[ "${CALIX_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml"
fi

install -Dm755 "$ROOT_DIR/target/release/calix" "$BIN_DIR/calix"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.desktop" "$DATA_DIR/applications/$APP_ID.desktop"
# dist ships `Exec=calix` for distro packages that install to /usr/bin. A graphical
# session rarely has $BIN_DIR on PATH, so point the entry at the binary we installed.
sed -i "s|^Exec=calix\$|Exec=$BIN_DIR/calix|" "$DATA_DIR/applications/$APP_ID.desktop"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.svg" "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "$ROOT_DIR/dist/$APP_ID.metainfo.xml" "$DATA_DIR/metainfo/$APP_ID.metainfo.xml"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$DATA_DIR/icons/hicolor" >/dev/null 2>&1 || true
fi

cat <<EOF
Calix installed.

Build: $("$BIN_DIR/calix" --version)
Binary: $BIN_DIR/calix
Desktop entry: $DATA_DIR/applications/$APP_ID.desktop

Make sure $BIN_DIR is on PATH if you want to run 'calix' from a terminal.
A running instance keeps the old binary until you restart it.
Check for drift later with scripts/check-installed.sh.
EOF
