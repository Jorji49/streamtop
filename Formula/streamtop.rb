# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v0.3.2.tar.gz"
  sha256 "8c8f962db62826eac09f53881ebe854490b330e029bc48a4fe3f0497cf0f998c"
  license "MIT"
  head "https://github.com/Jorji49/streamtop.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "streamtop", shell_output("#{bin}/streamtop --help")
  end
end
