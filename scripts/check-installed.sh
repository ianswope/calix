#!/usr/bin/env bash
# Reports whether the installed Calix matches the current checkout.
#
# `~/.local/bin/calix` is a copy, not a symlink, so a committed and tested fix
# stays invisible until scripts/install-local.sh runs again. Nothing used to say
# so: on 2026-07-30 the installed binary was seven commits and three days behind,
# and the missing fixes read as bugs that never got fixed.
#
# Exit status: 0 in sync, 1 drifted, 2 can't tell.
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
BIN="$BIN_DIR/calix"

if [[ ! -x "$BIN" ]]; then
  echo "Not installed: $BIN does not exist."
  echo "Run scripts/install-local.sh to install it."
  exit 2
fi

# "calix 0.4.0 (v0.4.0-24-ga2ab381 2026-07-30)" -> "v0.4.0-24-ga2ab381"
installed_line="$("$BIN" --version 2>/dev/null)"
installed_describe="$(sed -n 's/.*(\([^ )]*\).*/\1/p' <<<"$installed_line")"

if [[ -z "$installed_describe" ]]; then
  echo "Installed: $installed_line"
  echo "That build carries no commit stamp — it predates build.rs, so it is"
  echo "older than this check. Reinstall to get a comparable build."
  exit 1
fi

head_describe="$(git -C "$ROOT_DIR" describe --always --tags --dirty 2>/dev/null)"
if [[ -z "$head_describe" ]]; then
  echo "Installed: $installed_describe"
  echo "Cannot compare: $ROOT_DIR is not a git checkout."
  exit 2
fi

if [[ "$installed_describe" == "$head_describe" ]]; then
  echo "In sync: $installed_line"
  exit 0
fi

echo "DRIFTED"
echo "  installed: $installed_describe"
echo "  checkout:  $head_describe"

# Strip the `-dirty` suffix and the `<tag>-<n>-g` prefix to get a revision git
# can resolve. A describe with no tags is already a bare hash.
installed_rev="${installed_describe%-dirty}"
installed_rev="${installed_rev##*-g}"

if git -C "$ROOT_DIR" cat-file -e "${installed_rev}^{commit}" 2>/dev/null; then
  missing="$(git -C "$ROOT_DIR" log --oneline "$installed_rev..HEAD" 2>/dev/null)"
  if [[ -n "$missing" ]]; then
    echo
    echo "Committed but NOT installed:"
    sed 's/^/  /' <<<"$missing"
  fi
fi

echo
echo "Run scripts/install-local.sh to install the current checkout."
exit 1
