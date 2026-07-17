#!/bin/sh
set -eu

repo="fahmiirsyadk/torii"
install_root="${TORII_HOME:-${XDG_DATA_HOME:-"$HOME/.local/share"}/torii}"
bin_dir="${TORII_BIN_DIR:-"$HOME/.local/bin"}"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64) target="aarch64-unknown-linux-gnu" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  *) echo "Torii does not publish a build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tag="${TORII_VERSION:-}"
if [ -z "$tag" ]; then
  latest="$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/$repo/releases/latest")"
  tag="${latest##*/}"
fi
version="${tag#v}"
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$' || {
  echo "Invalid Torii version: $version" >&2
  exit 1
}
asset="torii-v${version}-${target}.tar.gz"
base="https://github.com/$repo/releases/download/v${version}"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

curl --proto '=https' --tlsv1.2 -fL "$base/$asset" -o "$temporary/$asset"
curl --proto '=https' --tlsv1.2 -fL "$base/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected="$(awk -v asset="$asset" '$2 == asset { print $1 }' "$temporary/SHA256SUMS")"
[ -n "$expected" ] || { echo "No checksum was published for $asset" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary/$asset" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$temporary/$asset" | awk '{ print $1 }')"
else
  echo "A SHA-256 implementation (sha256sum or shasum) is required" >&2
  exit 1
fi
[ "$actual" = "$expected" ] || { echo "Checksum verification failed for $asset" >&2; exit 1; }

version_dir="$install_root/versions/$version"
mkdir -p "$version_dir" "$install_root/bin" "$bin_dir"
tar -xzf "$temporary/$asset" -C "$version_dir"
[ -x "$version_dir/bin/torii" ] || { echo "Release is missing bin/torii" >&2; exit 1; }
[ -x "$version_dir/libexec/torii-sidecar" ] || {
  echo "Release is missing libexec/torii-sidecar" >&2
  exit 1
}

cp "$version_dir/bin/torii" "$install_root/bin/torii.new"
chmod 755 "$install_root/bin/torii.new"
mv -f "$install_root/bin/torii.new" "$install_root/bin/torii"
if [ -f "$install_root/current" ]; then
  cp "$install_root/current" "$install_root/.previous.new"
  mv -f "$install_root/.previous.new" "$install_root/previous"
fi
printf '%s\n' "$version" > "$install_root/.current.new"
mv -f "$install_root/.current.new" "$install_root/current"
printf '%s\n' "$version" > "$install_root/.pending.new"
mv -f "$install_root/.pending.new" "$install_root/pending"
ln -sfn "$install_root/bin/torii" "$bin_dir/torii"

echo "Installed Torii v$version at $install_root"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add $bin_dir to PATH to run torii." ;;
esac
