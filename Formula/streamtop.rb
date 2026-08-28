# frozen_string_literal: true

class Streamtop < Formula
  desc "HLS/DASH/IPTV stream diagnostics in the terminal"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "369127ebf4a72dfa23c49e53a35cfecda11c5cb3cd4a398a10f8b121e4241aff"
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
