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
cp "$root/deploy/linux/packages/"{nfpm.yaml,recog-setup,recog-setup.desktop,recog.desktop,recog.svg} .
export RECOG_VERSION=${tag#v}
./nfpm package -p deb -t "$out/recog-amd64.deb"
./nfpm package -p archlinux -t "$out/recog-x86_64.pkg.tar.zst"
cd "$out"
sha256sum recog-amd64.deb recog-x86_64.pkg.tar.zst > linux-packages.sha256
