# frozen_string_literal: true

class Streamtop < Formula
  desc "HLS/DASH/IPTV stream diagnostics in the terminal"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.7.1.tar.gz"
  sha256 "73098f853919714fcb508517d342b2c5122b5ef248bbd19a03205fb460c606e2"
  license "MIT"
  head "https://github.com/Jorji49/streamtop.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    generate_completions_from_executable(bin/"streamtop", "completions")
    (man1/"streamtop.1").write Utils.safe_popen_read(bin/"streamtop", "man")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/streamtop --version")
    assert_match "streamtop", shell_output("#{bin}/streamtop --help")
  end
end
