#!/usr/bin/env python3
"""Fail when flatpak/cargo-sources.json does not match Cargo.lock.

Flathub builds with no network. The manifest vendors every locked crate
through that file, so a lockfile change that skips the generator leaves
the offline build unable to find a crate.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = ROOT / "Cargo.lock"
SOURCES_PATH = ROOT / "flatpak" / "cargo-sources.json"
MANIFEST_PATH = ROOT / "flatpak" / "com.ianswope.Calix.json"


def locked_crates(text: str) -> dict[str, tuple[str, str]]:
    """Checksum -> (name, version) for every registry crate in the lockfile."""
    crates: dict[str, tuple[str, str]] = {}
    for block in re.split(r"\n\[\[package\]\]\n", text):
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        version = re.search(r'^version = "([^"]+)"', block, re.M)
        checksum = re.search(r'^checksum = "([^"]+)"', block, re.M)
        if name and version and checksum:
            crates[checksum.group(1)] = (name.group(1), version.group(1))
    return crates


def vendored_crates(sources: list) -> dict[str, str]:
    """Checksum -> crate archive URL."""
    vendored: dict[str, str] = {}
    for item in sources:
        if item.get("type") != "archive":
            continue
        digest = item.get("sha256")
        url = item.get("url", "")
        if digest:
            vendored[digest] = url
    return vendored


def main() -> int:
    manifest = json.loads(MANIFEST_PATH.read_text())
    referenced = any(
        source == "cargo-sources.json"
        for module in manifest["modules"]
        for source in module.get("sources", [])
    )
    if not referenced:
        print("flatpak manifest does not include cargo-sources.json", file=sys.stderr)
        return 1
    if not SOURCES_PATH.is_file():
        print(
            "flatpak/cargo-sources.json is missing; run scripts/generate-flatpak-sources.sh",
            file=sys.stderr,
        )
        return 1

    locked = locked_crates(LOCK_PATH.read_text())
    vendored = vendored_crates(json.loads(SOURCES_PATH.read_text()))
    missing = []
    for digest, (name, version) in sorted(locked.items(), key=lambda item: item[1]):
        url = vendored.get(digest)
        expected = f"/{name}/{name}-{version}.crate"
        if url is None or not url.endswith(expected):
            missing.append(f"{name} {version}")
    extra = [
        url
        for digest, url in sorted(vendored.items())
        if digest not in locked
    ]
    if missing or extra:
        if missing:
            print(
                "cargo-sources.json is missing locked crates:",
                ", ".join(missing),
                file=sys.stderr,
            )
        if extra:
            print(
                "cargo-sources.json vendors crates that are not in Cargo.lock:",
                ", ".join(extra),
                file=sys.stderr,
            )
        print("run scripts/generate-flatpak-sources.sh", file=sys.stderr)
        return 1
    print(f"flatpak cargo sources match Cargo.lock ({len(locked)} crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
