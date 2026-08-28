#!/bin/sh
# fetch-or-build.sh — herdr [[build]] step for worktree-setup.
#
# Fast path: download the prebuilt binary for this source's version + platform
# from the matching GitHub release and verify its SHA-256, so installing needs
# no Rust toolchain. When there is no prebuilt binary for this platform or
# version, fall back to `cargo build --release --locked`.
#
# SECURITY. The checksum is fetched from the same release as the asset, so it
# proves the download was not corrupted in transit — it does NOT prove who
# produced it. What it does buy: a mismatch is treated as tampering and ABORTS
# the install; it never silently degrades to a source build. Transport is
# pinned to https (including across redirects) so a redirect cannot downgrade
# the download to plaintext. See SECURITY.md for the full trust model and for
# `gh attestation verify`, which does prove provenance.
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
  # herdr may have been launched without ~/.cargo/bin on PATH — and possibly
  # without HOME at all, which `set -u` would otherwise turn into a hard error.
  if [ -n "${HOME:-}" ] && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
  fi
  if ! have cargo; then
    echo "$bin: no prebuilt binary for this platform/version and cargo was not found." >&2
    echo "Install Rust (https://rustup.rs) and re-run: herdr plugin install $repo" >&2
    exit 1
  fi
  echo "$bin: building from source with cargo…" >&2
  cd "$root" && exec cargo build --release --locked
}

# A miss: nothing suspicious, just nothing to download.
fallback() {
  echo "$bin: $1 — building from source instead." >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  build_from_source
}

# Tampering, or something pretending to be this release. Never proceed.
refuse() {
  echo "$bin: REFUSING TO INSTALL — $1" >&2
  echo "  The download did not match the checksum published for v$version." >&2
  echo "  Nothing was installed. Report this at https://github.com/$repo/security/advisories" >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  exit 1
}

download() { # url dest
  if have curl; then
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
         --location --max-redirs 5 --retry 3 --retry-connrefused \
         --fail --silent --show-error -o "$2" "$1"
  elif have wget; then
    wget --https-only --secure-protocol=TLSv1_2 --max-redirect=5 --tries=3 -q -O "$2" "$1"
  else
    return 127
  fi
}

sha256_of() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else return 127
  fi
}

version=$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$version" ] || fallback "could not read the version from Cargo.toml"

# A plaintext or non-http origin would make the checksum meaningless.
case "$base_url" in
  https://*) ;;
  *) fallback "refusing a non-https download origin ($base_url)" ;;
esac

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
if [ "$expected" != "$actual" ]; then
  echo "  expected $expected" >&2
  echo "  actual   $actual" >&2
  refuse "checksum mismatch for $asset"
fi

# Extract only the one member we expect, and never restore an archive's
# ownership or permission bits.
tar -xzf "$tmp/$asset" -C "$tmp" --no-same-owner --no-same-permissions "$bin" \
  || refuse "$asset did not contain exactly $bin"
[ -f "$tmp/$bin" ] || refuse "$asset did not contain $bin"
mkdir -p "$(dirname "$out")" || fallback "cannot create $(dirname "$out")"
mv "$tmp/$bin" "$out" && chmod 755 "$out" || fallback "could not install $out"
rm -rf "$tmp"
echo "$bin: installed prebuilt v$version ($triple) → $out"
echo "$bin: verify its provenance with: gh attestation verify --repo $repo $asset"
