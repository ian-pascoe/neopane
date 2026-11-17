#!/usr/bin/env bash
# Installs neopane from a GitHub release, on macOS or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/spiritledsoftware/neopane/main/install.sh | bash
#
# NEOPANE_VERSION     a release tag, such as v0.1.0 (default: the latest)
# NEOPANE_INSTALL_DIR where neopane goes, with npane linked to it
#                     (default: ~/.local/bin)
# NEOPANE_BASE_URL    where the archives are (default: the release's
#                     download URL; CI points it at its own builds)
set -euo pipefail

repo="spiritledsoftware/neopane"
version="${NEOPANE_VERSION:-latest}"
install_dir="${NEOPANE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'neopane install: %s\n' "$*" >&2; }
die() {
  say "$*"
  exit 1
}

case "$(uname -s)" in
Darwin) os=apple-darwin ;;
Linux) os=unknown-linux-musl ;;
*) die "no release for $(uname -s); build it with cargo install --locked neopane" ;;
esac
case "$(uname -m)" in
arm64 | aarch64) arch=aarch64 ;;
x86_64 | amd64) arch=x86_64 ;;
*) die "no release for $(uname -m); build it with cargo install --locked neopane" ;;
esac
target="$arch-$os"
archive="neopane-$target.tar.gz"

if [ -n "${NEOPANE_BASE_URL:-}" ]; then
  base="$NEOPANE_BASE_URL"
elif [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

command -v curl >/dev/null || die "needs curl"
command -v tar >/dev/null || die "needs tar"
if command -v sha256sum >/dev/null; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "needs sha256sum or shasum to check the download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading $base/$archive"
curl -fsSL --retry 3 -o "$tmp/$archive" "$base/$archive" ||
  die "couldn't download $base/$archive"
curl -fsSL --retry 3 -o "$tmp/$archive.sha256" "$base/$archive.sha256" ||
  die "couldn't download $base/$archive.sha256"
want="$(cut -d' ' -f1 <"$tmp/$archive.sha256")"
got="$(sha256 "$tmp/$archive")"
[ "$want" = "$got" ] || die "$archive's checksum is $got, not $want"

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/neopane" ] || die "$archive has no neopane in it"

mkdir -p "$install_dir"
# A new file moved over the old one: on macOS, a binary written over in
# place is killed at launch
cp "$tmp/neopane" "$install_dir/.neopane.new"
chmod 755 "$install_dir/.neopane.new"
mv -f "$install_dir/.neopane.new" "$install_dir/neopane"
ln -sfn neopane "$install_dir/npane"

installed="$("$install_dir/neopane" --version)" || die "$install_dir/neopane doesn't run"
say "installed ${installed%%$'\n'*} as $install_dir/neopane, and npane"
case ":$PATH:" in
*":$install_dir:"*) ;;
*) say "$install_dir isn't on your PATH; add it to use neopane" ;;
esac
command -v nvim >/dev/null || say "neopane needs Neovim 0.12 or later, as nvim on PATH"
say "a running daemon keeps the binary it started with: start a new one to use this"
