# frozen_string_literal: true

class Streamtop < Formula
  desc "HLS/DASH/IPTV stream diagnostics in the terminal"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.8.0.tar.gz"
  sha256 "5acb040d98fe50dba054d7c5f9f07062c13294099c2dc762679ebf3d71da9aea"
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
