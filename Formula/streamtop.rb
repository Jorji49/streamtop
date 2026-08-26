# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v0.3.2.tar.gz"
  sha256 "d633ed8933ee59f645e6be4a04443a1c1dd9f4c7bfe93465ac46b59cab3a6040"
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
