# frozen_string_literal: true

class Streamtop < Formula
  desc "HLS/DASH/IPTV stream diagnostics in the terminal"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.6.0.tar.gz"
  sha256 "0a1cdfea8a7411b98d5fe37ad32d8981c2b32e0a7feab1d0a5667d3a009f95cb"
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
