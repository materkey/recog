#!/bin/bash
# Fetch the full upstream distribution, including generate_appcast and sign_update.
set -euo pipefail
if [[ $# != 1 || -z $1 ]]; then
    echo "Usage: $0 SDK-DIRECTORY" >&2
    exit 2
fi
name=Sparkle-2.10.1.tar.xz
sha256=1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27
mkdir -p "$(dirname "$1")"
parent=$(cd "$(dirname "$1")" && pwd -P)
dest="$parent/$(basename "$1")"
tmp=$(mktemp -d "$parent/.sparkle-fetch.XXXXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
if [[ -f "$parent/$name" ]]; then
    cp "$parent/$name" "$name"
else
    curl --proto '=https' --proto-redir '=https' --location --fail --retry 3 \
        --output "$name" "https://github.com/sparkle-project/Sparkle/releases/download/2.10.1/$name"
fi
printf '%s  %s\n' "$sha256" "$name" | shasum -a 256 -c -
mkdir sdk
tar -xf "$name" -C sdk
if [[ -d $dest && ! -L $dest && -n $(ls -A "$dest") ]]; then
    # Dry run: accept an identical cached SDK, leave any different contents alone.
    changes=$(rsync -rlpci --dry-run --delete sdk/ "$dest/")
    if [[ -n $changes ]]; then
        echo "The existing directory differs from the pinned Sparkle SDK: $dest" >&2
        exit 1
    fi
else
    python3 -c 'import os, sys; os.rename("sdk", sys.argv[1])' "$dest"
fi
