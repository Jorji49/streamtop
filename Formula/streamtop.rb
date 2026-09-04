# frozen_string_literal: true

class Streamtop < Formula
  desc "HLS/DASH/IPTV stream diagnostics in the terminal"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.5.0.tar.gz"
  sha256 "ff194d164d432355e54f30edcb597e9fb55ea56ab100d2fb87262bd03a592722"
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
