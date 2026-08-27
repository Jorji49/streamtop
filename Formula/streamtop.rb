# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v0.3.3.tar.gz"
  sha256 "a5daeecd7859da506c8a5ac2141994699cbea909aab241d6171fefb38dc6907e"
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
