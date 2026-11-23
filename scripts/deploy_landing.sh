#!/usr/bin/env bash
# Publish deploy/landing of a release tag to the site. The web server on
# RECOG_SITE_HOST serves $RECOG_SITE_DIR/current, a link to one directory per
# deploy under releases/, so the switch is atomic and an earlier directory is
# the rollback. Running it again for the same tag only switches back to it.
# Both settings live outside the repository, in ~/.config/recog/release.env.
set -euo pipefail
tag=${1:?usage: scripts/deploy_landing.sh vX.Y.Z}
host=${RECOG_SITE_HOST:?set RECOG_SITE_HOST (~/.config/recog/release.env)}
root=${RECOG_SITE_DIR:?set RECOG_SITE_DIR (~/.config/recog/release.env)}
site=https://recog.vkkovalev.work
cd "$(dirname "$0")/.."
commit=$(git rev-parse --verify "$tag^{commit}")
id="$tag-${commit:0:12}"
archive="/tmp/recog-landing-$id.tar"
# shellcheck disable=SC2029 # $id and $archive are meant to expand here, on the client.
git archive "$tag" deploy/landing | ssh "$host" "cat > '$archive'"
# shellcheck disable=SC2029
ssh "$host" "bash -s -- '$id' '$archive' '$root'" <<'REMOTE'
set -euo pipefail
id=$1 archive=$2
cd "$3"
if [ ! -d "releases/$id" ]; then
    stage=$(mktemp -d)
    tar -x -C "$stage" -f "$archive"
    mv "$stage/deploy/landing" "releases/$id"
    rm -rf "$stage"
    chmod -R a+rX "releases/$id"
fi
rm -f "$archive"
ln -sfn "releases/$id" current.new
mv -T current.new current
echo "current -> $(readlink current)"
REMOTE
for file in index.html install.sh recog.asc site.css; do
    if ! git show "$tag:deploy/landing/$file" | cmp -s - <(curl -qfsSL "$site/$file"); then
        echo "$site/$file differs from $tag" >&2
        exit 1
    fi
done
echo "$site serves deploy/landing of $tag"
