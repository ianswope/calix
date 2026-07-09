#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v flatpak-cargo-generator >/dev/null 2>&1; then
  echo "flatpak-cargo-generator is required; install it with pipx or your distribution package manager." >&2
  exit 1
fi

flatpak-cargo-generator "$ROOT_DIR/Cargo.lock" -o "$ROOT_DIR/flatpak/cargo-sources.json"
