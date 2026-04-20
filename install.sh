#!/usr/bin/env bash
set -euo pipefail

repo="${ASC_SYNC_INSTALL_REPO:-iivankin/asc-sync}"
install_dir="${ASC_SYNC_INSTALL_DIR:-$HOME/.local/bin}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Linux)
    os="linux"
    ext="tar.gz"
    ;;
  Darwin)
    os="macos"
    ext="tar.gz"
    ;;
  *)
    echo "unsupported OS: $uname_s" >&2
    exit 1
    ;;
esac

case "$uname_m" in
  x86_64|amd64)
    arch="x86_64"
    ;;
  arm64|aarch64)
    arch="arm64"
    ;;
  *)
    echo "unsupported architecture: $uname_m" >&2
    exit 1
    ;;
esac

asset="asc-sync-${os}-${arch}.${ext}"

case "$asset" in
  asc-sync-linux-x86_64.tar.gz|asc-sync-macos-arm64.tar.gz)
    ;;
  *)
    echo "no published binary for ${os}-${arch}" >&2
    exit 1
    ;;
esac

url="https://github.com/${repo}/releases/latest/download/${asset}"
tmp_dir="$(mktemp -d)"
archive_path="${tmp_dir}/${asset}"

cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

echo "downloading ${url}"
curl -fsSL "$url" -o "$archive_path"

mkdir -p "$install_dir"
tar -xzf "$archive_path" -C "$tmp_dir"
install -m 0755 "${tmp_dir}/asc-sync" "${install_dir}/asc-sync"
ln -sf "asc-sync" "${install_dir}/ascs"

echo "installed asc-sync to ${install_dir}/asc-sync"
echo "installed ascs alias to ${install_dir}/ascs"
echo "make sure ${install_dir} is in PATH"
