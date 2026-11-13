class Calix < Formula
  desc "Native GTK4 calendar for Linux with Google, iCloud, and CalDAV sync"
  homepage "https://github.com/ianswope/calix"
  url "https://github.com/ianswope/calix/archive/refs/tags/v0.10.0.tar.gz"
  sha256 "bd45fc357a2457268d720ea10b53dc84162dde7f55d98f15b16aead5cb4e1852"
  license "MIT"
  head "https://github.com/ianswope/calix.git", branch: "master"

  depends_on "pkgconf" => :build
  depends_on "rust" => :build
  depends_on "gtk4"
  depends_on "libadwaita"

  def install
    system "cargo", "install", *std_cargo_args(path: ".")

    (share/"applications").install "dist/com.ianswope.Calix.desktop"
    (share/"icons/hicolor/scalable/apps").install "dist/com.ianswope.Calix.svg"
    (share/"metainfo").install "dist/com.ianswope.Calix.metainfo.xml"
    (share/"doc/calix").install "README.md", "CHANGELOG.md", "LICENSE"
  end

  test do
    assert_match(/^calix /, shell_output("#{bin}/calix --version"))
    assert_predicate share/"applications/com.ianswope.Calix.desktop", :exist?
    assert_predicate share/"metainfo/com.ianswope.Calix.metainfo.xml", :exist?
  end
end
