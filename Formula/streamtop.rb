# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal diagnostic engine for live HLS, DASH, and IPTV streams"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v0.3.0.tar.gz"
  sha256 "18c07c582bba6922a7bd35f286cf6a2960e7e1d6fe27d37b84eb6efe5459eaf0"
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
