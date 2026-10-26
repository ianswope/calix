# Release and packaging checklist

The order matters: several packaging artifacts reference the tag, so they can
only be finished once the tag exists and is immutable.

## 1. Cut the release

1. Bump `version` in `Cargo.toml`, then `cargo check` so `Cargo.lock` follows.
2. Rename the `## Unreleased` heading in `CHANGELOG.md` to `## X.Y.Z — DATE`
   and add a two- or three-sentence lede.
3. Add a matching `<release>` block at the top of
   `dist/com.ianswope.Calix.metainfo.xml`, and repoint the `<screenshot>` URLs
   at the new tag.
4. Bump `pkgver` in `packaging/aur/PKGBUILD` and the `tag` in
   `flatpak/com.ianswope.Calix.json`.
5. Run all four CI gates plus `scripts/check-package.sh`.
6. Commit, tag `vX.Y.Z`, push the tag. `.github/workflows/release.yml` builds
   the archive and publishes the GitHub release.

The screenshot URLs in the metainfo point at the tag, so they 404 between the
commit and the tag push. That is expected; verify them after pushing.

## 2. AUR

`packaging/aur/PKGBUILD` ships with `sha256sums=('SKIP')` and **must not be
published that way**. After the tag is pushed:

```sh
curl -sL https://github.com/ianswope/calix/archive/refs/tags/vX.Y.Z.tar.gz \
  | sha256sum
```

Put that hash in `sha256sums`, then `makepkg --printsrcinfo > .SRCINFO` and
push to the AUR remote. Verify with a clean `makepkg -si` in a container before
publishing — the PKGBUILD builds from source and has never been run on a
machine without the project's own toolchain already present.

## 3. Flathub

**Not ready to submit.** One thing still blocks it:

- `flatpak/cargo-sources.json` is generated, not committed. Flathub builders
  have no network, so the locked Cargo sources have to be vendored into the
  manifest by `scripts/generate-flatpak-sources.sh`, which needs
  `flatpak-cargo-generator` installed.

"Start Calix when you sign in" uses the Background portal's `RequestBackground`
when `/.flatpak-info` is present. The portal writes the login entry on the
host, and the switch follows the autostart bit in the portal's answer. Outside
a sandbox, Calix still writes `$XDG_CONFIG_HOME/autostart/com.ianswope.Calix.desktop`.

Flathub also requires the source pinned to a `commit` sha, not just a `tag`.
Add it at submission time:

```sh
git rev-parse vX.Y.Z^{commit}
```

The manifest targets `org.gnome.Platform` 50; GNOME 48 reached end of life on
2026-03-24, so it cannot be used. Check the runtime is still current at
submission time.

## 4. Homebrew

`Formula/calix.rb` is `--HEAD` only. A checksum-pinned stable formula needs a
`url`/`sha256` pair for the tagged archive; it is the lowest-value channel
here, since Calix needs a GTK4 session and Homebrew's Linux users mostly do not
have one.
