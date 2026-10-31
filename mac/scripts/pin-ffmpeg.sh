#!/usr/bin/env bash
# Points the app at an FFmpeg build published by .github/workflows/ffmpeg.yml:
# downloads it, works out its SHA-256 and size, and writes them with its URL
# to mac/Resources/ffmpeg-build.json, which build-app.sh puts in the app.
# The app checks every download against that SHA-256 before using it.
#
#   mac/scripts/pin-ffmpeg.sh ffmpeg-8.0.1-1 <SHA-256>
#
# The SHA-256 is the one the workflow run printed in its summary; giving it
# makes sure the download is the build that run made. On a Mac the script
# also shows who signed the programs. Commit the changed ffmpeg-build.json;
# the next app release uses it.
set -euo pipefail

TAG="${1:-}"
EXPECTED="$(tr '[:upper:]' '[:lower:]' <<< "${2:-}")"
usage() {
  echo "usage: pin-ffmpeg.sh ffmpeg-<version>-<build> [SHA-256], such as ffmpeg-8.0.1-1" >&2
  exit 2
}
[[ "$TAG" =~ ^ffmpeg-([0-9]+(\.[0-9]+)*)-[0-9]+$ ]] || usage
# Read before the next match, which replaces BASH_REMATCH.
VERSION="${BASH_REMATCH[1]}"
[[ -z "$EXPECTED" || "$EXPECTED" =~ ^[0-9a-f]{64}$ ]] || usage
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REMOTE="$(git -C "$ROOT" remote get-url origin 2> /dev/null || echo https://github.com/Suvraneel/Radlet.git)"
REPO="$(sed -E 's#^(https://github.com/|git@github.com:)##; s#\.git$##' <<< "$REMOTE")"
URL="https://github.com/$REPO/releases/download/$TAG/ffmpeg-$VERSION-macos.zip"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
echo "Downloading ${URL}…"
curl -fsSL --retry 3 -o "$WORK/ffmpeg.zip" "$URL"
if command -v sha256sum > /dev/null; then
  SUM="$(sha256sum "$WORK/ffmpeg.zip" | cut -d' ' -f1)"
else
  SUM="$(shasum -a 256 "$WORK/ffmpeg.zip" | cut -d' ' -f1)"
fi
SIZE="$(wc -c < "$WORK/ffmpeg.zip" | tr -d ' ')"
if [[ -n "$EXPECTED" && "$SUM" != "$EXPECTED" ]]; then
  echo "The download's SHA-256 is $SUM, not $EXPECTED. Nothing was pinned." >&2
  exit 1
fi
[[ -z "$EXPECTED" ]] && echo "warning: no SHA-256 given to compare the download with." >&2
# A quick look inside: the two programs, for both chips when lipo is here.
unzip -q "$WORK/ffmpeg.zip" -d "$WORK/unpacked"
for tool in ffmpeg ffprobe; do
  [[ -x "$WORK/unpacked/$tool" ]] || { echo "The download has no $tool." >&2; exit 1; }
  if command -v lipo > /dev/null; then
    lipo "$WORK/unpacked/$tool" -verify_arch arm64 x86_64
  fi
done
if command -v codesign > /dev/null; then
  signer="$(codesign -dv --verbose=2 "$WORK/unpacked/ffmpeg" 2>&1 | sed -nE 's/^(Authority|TeamIdentifier)=/\1: /p')"
  echo "Signed by:"
  echo "${signer:-  nobody (ad-hoc)}"
fi

cat > "$ROOT/mac/Resources/ffmpeg-build.json" << EOF
{
  "version": "$VERSION",
  "url": "$URL",
  "sha256": "$SUM",
  "size": $SIZE
}
EOF
echo "Pinned FFmpeg $VERSION ($SIZE bytes, SHA-256 $SUM) in mac/Resources/ffmpeg-build.json."
