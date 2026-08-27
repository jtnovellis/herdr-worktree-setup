#!/bin/sh
# fetch-or-build.sh — herdr [[build]] step for worktree-setup.
#
# Fast path: download the prebuilt binary for this source's version + platform
# from the matching GitHub release and verify its SHA-256, so installing needs
# no Rust toolchain. On ANY miss (no release for this version, unsupported
# platform, download or checksum failure) fall back to `cargo build --release`.
#
# Everything is resolved relative to this script: herdr runs the build in a
# staging checkout that it renames afterwards, so absolute paths must not be
# baked in anywhere.
set -u

repo="jtnovellis/herdr-worktree-setup"
bin="herdr-worktree-setup"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=${HWS_REPO_ROOT:-"$script_dir/.."}
out="$root/target/release/$bin"
base_url=${HWS_BASE_URL:-"https://github.com/$repo/releases/download"}

have() { command -v "$1" >/dev/null 2>&1; }

build_from_source() {
  # herdr may have been launched without ~/.cargo/bin on PATH.
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "$bin: no prebuilt binary for this platform/version and cargo was not found." >&2
    echo "Install Rust (https://rustup.rs) and re-run: herdr plugin install $repo" >&2
    exit 1
  fi
  echo "$bin: building from source with cargo…" >&2
  cd "$root" && exec cargo build --release --locked
}

fallback() {
  echo "$bin: $1 — building from source instead." >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  build_from_source
}

download() { # url dest
  if have curl; then curl -fsSL -o "$2" "$1"
  elif have wget; then wget -q -O "$2" "$1"
  else return 127; fi
}

sha256_of() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else return 127; fi
}

version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$version" ] || fallback "could not read the version from Cargo.toml"

os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64) triple=aarch64-apple-darwin ;;
  Darwin:x86_64)               triple=x86_64-apple-darwin ;;
  Linux:x86_64)                triple=x86_64-unknown-linux-musl ;;
  Linux:aarch64|Linux:arm64)   triple=aarch64-unknown-linux-musl ;;
  *) fallback "no prebuilt binary for $os/$arch" ;;
esac

asset="$bin-$triple.tar.gz"
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t hws) || fallback "cannot create a temp dir"
url="$base_url/v$version/$asset"

download "$url" "$tmp/$asset" || fallback "download of $url failed"
download "$base_url/v$version/SHA256SUMS" "$tmp/SHA256SUMS" || fallback "download of SHA256SUMS failed"

expected=$(grep -E "^[0-9a-f]{64} [ *]$asset\$" "$tmp/SHA256SUMS" | head -n1 | awk '{print $1}')
[ -n "$expected" ] || fallback "no checksum for $asset in SHA256SUMS"
actual=$(sha256_of "$tmp/$asset") || fallback "no sha256 tool available"
[ "$expected" = "$actual" ] || fallback "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp" || fallback "could not extract $asset"
[ -f "$tmp/$bin" ] || fallback "$asset did not contain $bin"
mkdir -p "$(dirname "$out")" || fallback "cannot create $(dirname "$out")"
mv "$tmp/$bin" "$out" && chmod +x "$out" || fallback "could not install $out"
rm -rf "$tmp"
echo "$bin: installed prebuilt v$version ($triple) → $out"
