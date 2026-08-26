# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v0.3.0.tar.gz"
  sha256 "57d6ed3c22be77a84545612ae949d005b5fdd42dd4305035c2d11cdcec32a575"
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
