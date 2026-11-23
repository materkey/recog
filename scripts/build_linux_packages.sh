#!/usr/bin/env bash
# Package the same release tree used by the standalone installer.
set -euo pipefail
tag=${1:?expected release tag}
out=$(cd "${2:?expected archive directory}" && pwd)
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
cd "$work"
curl -qfsSL --retry 3 https://github.com/goreleaser/nfpm/releases/download/v2.49.0/nfpm_2.49.0_Linux_x86_64.tar.gz -o nfpm.tar.gz
echo '2afaa5ffb5e3493402e31e7c67a5a1fb42133e3f66fc315ca0051934125561c7  nfpm.tar.gz' | sha256sum -c -
tar -xzf nfpm.tar.gz nfpm
tar -xzf "$out/recog-$tag-linux-x86_64.tar.gz"
[[ $(cat recog/recog-release) == "$tag" ]]
cp "$root/deploy/linux/packages/"* .
export RECOG_VERSION=${tag#v}
# Versioned names: pacman keeps downloads in its cache by file name.
./nfpm package -p deb -t "$out/"
./nfpm package -p archlinux -t "$out/"
cd "$out"
sha256sum recog_*_amd64.deb recog-*-x86_64.pkg.tar.zst > linux-packages.sha256
python3 "$root/scripts/linux_repo.py" index "$out"
