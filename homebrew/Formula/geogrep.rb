class Geogrep < Formula
  desc "Fuzzy grep for geospatial vector data"
  homepage "https://github.com/jesperfjellin/geogrep"
  url "https://github.com/jesperfjellin/geogrep/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "c1d96754dab0f59282f2d497166f2ea21d0065a6cd368a7a173b8b0c8c9f8592"
  license "MIT"
  head "https://github.com/jesperfjellin/geogrep.git", branch: "main"

  depends_on "pkg-config" => :build
  depends_on "rust" => :build
  depends_on "gdal"

  def install
    gdal = Formula["gdal"]
    ENV["GDAL_HOME"] = gdal.opt_prefix.to_s
    ENV["GDAL_INCLUDE_DIR"] = gdal.opt_include.to_s
    ENV["GDAL_LIB_DIR"] = gdal.opt_lib.to_s
    ENV["GDAL_VERSION"] = gdal.version.to_s
    ENV.append "RUSTFLAGS", "-C link-arg=-Wl,-rpath,#{gdal.opt_lib}" if OS.linux?

    system "cargo", "install", *std_cargo_args(path: "crates/geogrep"), "--bin", "gg"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/gg --version")
  end
end
