#!/bin/bash
# Build the standalone audio converter bundled in the macOS arm64 disk image.
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin LC_ALL=C

if [[ $# != 1 || -z $1 ]]; then
    echo "Usage: $0 <output-directory>" >&2
    exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'Build on an arm64 Mac with the Apple command line tools.' >&2
    exit 1
fi

version=9.0.2
archive="ffmpeg-$version.tar.xz"
url="https://ffmpeg.org/releases/$archive"
# Verified against the detached upstream signature from FFmpeg release signing key
# FCF986EA15E6E293A5644F10B4322F04D67658D8 (https://ffmpeg.org/download.html).
sha256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
recipe=$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")
mkdir -p "$1"
out=$(cd "$1" && pwd -P)
work=$(mktemp -d /tmp/recog-dmg-ffmpeg.XXXXXXXX)
trap 'rm -rf "$work"' EXIT
licenses="$work/licenses"
mkdir "$licenses"
cd "$licenses"
if [[ -f ${recipe%/*}/$archive ]]; then
    cp "${recipe%/*}/$archive" "$archive"
else
    curl --proto '=https' --proto-redir '=https' --fail --location --retry 3 --output "$archive" "$url"
fi
printf '%s  %s\n' "$sha256" "$archive" > SHA256SUMS
shasum -a 256 -c SHA256SUMS
tar -xf "$archive" -C "$work"
cd "$work/ffmpeg-$version"

# Configure and compile with just Apple's toolchain. In particular, do not inherit
# a package manager's header/library search paths, compiler flags or pkg-config.
build_env=(/usr/bin/env -i "PATH=$PATH" "HOME=$work" LC_ALL=C
    "DEVELOPER_DIR=${DEVELOPER_DIR:-$(xcode-select -p)}"
    "SDKROOT=$(xcrun --sdk macosx --show-sdk-path)"
    MACOSX_DEPLOYMENT_TARGET=13.0 SOURCE_DATE_EPOCH=1789699562 ZERO_AR_DATE=1)
"${build_env[@]}" ./configure \
    --arch=aarch64 --target-os=darwin --cc=clang --pkg-config=false \
    --extra-cflags='-mmacosx-version-min=13.0 -Werror=unguarded-availability-new' \
    --extra-ldflags=-mmacosx-version-min=13.0 \
    --enable-static --disable-shared --disable-autodetect \
    --disable-gpl --disable-version3 --disable-nonfree \
    --disable-debug --disable-doc --disable-programs --enable-ffmpeg \
    --disable-network --disable-avdevice --disable-swscale --disable-everything \
    --enable-protocol=file,pipe \
    --enable-demuxer=wav,w64,mov,aac,loas,mp3,flac,ogg,matroska,aiff,caf,ac3,eac3,ape,wv \
    --enable-decoder='pcm_*,aac,aac_latm,alac,mp1float,mp2float,mp3float,flac,vorbis,opus,ac3,eac3,ape,wavpack,adpcm_ima_wav,adpcm_ms,adpcm_ima_qt' \
    --enable-parser=aac,aac_latm,mpegaudio,flac,vorbis,opus,ac3 \
    --enable-filter=aresample --enable-encoder=pcm_s16le --enable-muxer=wav
for setting in 'STATIC 1' 'SHARED 0' 'AUTODETECT 0' 'GPL 0' 'VERSION3 0' 'NONFREE 0'; do
    grep -Fxq "#define CONFIG_$setting" config.h
done
"${build_env[@]}" make -j"$(sysctl -n hw.logicalcpu)" ffmpeg

codesign --force --sign - --timestamp=none --identifier org.ffmpeg.ffmpeg ffmpeg
codesign --verify --strict ffmpeg
[[ $(lipo -archs ffmpeg) == arm64 ]]
vtool -show-build ffmpeg | awk '$1 == "platform" { mac = ($2 == "MACOS") }
    $1 == "minos" { target = ($2 == "13.0") } END { exit !(mac && target) }'
otool -L ffmpeg | awk 'NR == 1 { next } $1 ~ /^\/usr\/lib\// || $1 ~ /^\/System\/Library\/Frameworks\// { next }
    { print "Unexpected dependency: " $0 > "/dev/stderr"; bad = 1 } END { exit bad || NR < 2 }'
./ffmpeg -hide_banner -L > "$licenses/ffmpeg-license.txt"
tr '\n' ' ' < "$licenses/ffmpeg-license.txt" | grep -Fq 'GNU Lesser General Public License'
grep -Fq 'version 2.1 of the License' "$licenses/ffmpeg-license.txt"

cp COPYING.LGPLv2.1 LICENSE.md CREDITS "$licenses/"
install -m 755 "$recipe" "$licenses/build_dmg_ffmpeg.sh"
{
    xcrun clang --version
    printf 'macOS SDK %s (build %s)\n' "$(xcrun --sdk macosx --show-sdk-version)" "$(xcrun --sdk macosx --show-sdk-build-version)"
    ./ffmpeg -hide_banner -buildconf
} > "$licenses/build-info.txt" 2>&1
cat > "$licenses/NOTICE.txt" <<NOTICE
FFmpeg $version, Copyright (c) the FFmpeg developers. https://ffmpeg.org/
Recog runs this standalone executable as a subprocess. Its built-in FFmpeg
libraries are statically linked. The combined executable is distributed under
the GNU Lesser General Public License, version 2.1 or later (COPYING.LGPLv2.1).
LICENSE.md describes upstream licensing. CREDITS and individual source files
identify contributors and additional notices. FFmpeg is a trademark of Fabrice Bellard.

Complete corresponding upstream source, unmodified: $archive
Official source: $url
Official detached signature: $url.asc
Signature verified when pinning SHA-256 with FFmpeg release signing key
FCF986EA15E6E293A5644F10B4322F04D67658D8. See SHA256SUMS.

Rebuild offline from this directory on an arm64 Mac with Apple command line tools:
    ./build_dmg_ffmpeg.sh /path/to/output
The included script contains the configure flags, build and ad hoc signing steps.
build-info.txt records the compiler, SDK build and applied configure options.
NOTICE

mkdir -p "$out/licenses"
cp -p "$licenses/"* "$out/licenses/"
install -m 755 ffmpeg "$out/ffmpeg"
