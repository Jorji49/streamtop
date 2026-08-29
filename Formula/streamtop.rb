# frozen_string_literal: true

class Streamtop < Formula
  desc "Terminal HLS, DASH, and IPTV stream monitor with wire probes and metrics"
  homepage "https://github.com/Jorji49/streamtop"
  url "https://github.com/Jorji49/streamtop/archive/refs/tags/v1.1.1.tar.gz"
  sha256 "15929c66f93795f8d58297597fd32def932b8b724f260743750f35f2743797a4"
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
