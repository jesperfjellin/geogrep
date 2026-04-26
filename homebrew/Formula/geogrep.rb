class Geogrep < Formula
  desc "Fuzzy grep for geospatial vector data"
  homepage "https://github.com/jesperfjellin/geogrep"
  url "https://github.com/jesperfjellin/geogrep/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "REPLACE_WITH_SHA256_OF_v1.0.0_TARBALL"
  license "MIT"
  head "https://github.com/jesperfjellin/geogrep.git", branch: "main"

  depends_on "pkg-config" => :build
  depends_on "rust" => :build
  depends_on "gdal"

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/geogrep"), "--bin", "gg"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/gg --version")
  end
end
