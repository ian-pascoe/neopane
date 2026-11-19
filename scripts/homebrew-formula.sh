#!/bin/sh
# Prints the Homebrew formula of a release, Formula/neopane.rb in
# spiritledsoftware/homebrew-tap, which .github/workflows/homebrew.yml
# replaces with it on every release. Change the formula here, never in the
# tap: the next release overwrites it.
#
# SUMS is the release's SHA256SUMS, one `SUM  neopane-TARGET.tar.gz` line
# per archive release.yml builds.
#
#   scripts/homebrew-formula.sh VERSION SUMS
set -eu
if [ $# -ne 2 ]; then
  echo "usage: $0 VERSION SUMS" >&2
  exit 2
fi
version=$1
sums=$2
case $version in
v*)
  echo "$0: VERSION is bare, such as 0.2.2, not $version" >&2
  exit 2
  ;;
esac

base="https://github.com/spiritledsoftware/neopane/releases/download/v$version"

sum() {
  s=$(awk -v f="neopane-$1.tar.gz" '$2 == f { print $1 }' "$sums")
  case $s in
  *[!0-9a-f]* | "")
    echo "$0: no sha256 of neopane-$1.tar.gz in $sums" >&2
    exit 1
    ;;
  esac
  [ ${#s} -eq 64 ] || {
    echo "$0: bad sha256 of neopane-$1.tar.gz in $sums: $s" >&2
    exit 1
  }
  echo "$s"
}

mac_arm=$(sum aarch64-apple-darwin)
mac_intel=$(sum x86_64-apple-darwin)
linux_arm=$(sum aarch64-unknown-linux-musl)
linux_intel=$(sum x86_64-unknown-linux-musl)

cat <<EOF
class Neopane < Formula
  desc "One Neovim in the background, shown in every terminal attached to it"
  homepage "https://github.com/spiritledsoftware/neopane"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "$base/neopane-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    else
      url "$base/neopane-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "$base/neopane-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$linux_arm"
    else
      url "$base/neopane-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "neopane"
    bin.install_symlink "neopane" => "npane"
  end

  def caveats
    <<~EOS
      neopane needs Neovim 0.12 or later, as nvim on PATH.
      A running daemon keeps the binary it started with: after an upgrade,
      end it (neopane kill) and start a new one.
    EOS
  end

  test do
    assert_match "neopane #{version}", shell_output("#{bin}/neopane --version")
    assert_match "neopane #{version}", shell_output("#{bin}/npane --version")
  end
end
EOF
