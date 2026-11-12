#!/bin/sh
# Downloads TailPocket's release files into vendor/tailpocket and checks each
# against the release's SHA256SUMS. Binaries are not committed.
#
#   scripts/fetch-vendor.sh                 # this machine's platform
#   scripts/fetch-vendor.sh windows-amd64   # another platform (CI packaging)
#   scripts/fetch-vendor.sh mobile          # phone app packages only (EAS builds)
#
# TAILPOCKET_VERSION overrides the pinned version.
set -eu

VERSION="${TAILPOCKET_VERSION:-v0.2.2}"
BASE="https://github.com/jt-wang/tailpocket/releases/download/$VERSION"
API="https://api.github.com/repos/jt-wang/tailpocket/releases/tags/$VERSION"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/vendor/tailpocket"

if [ $# -ge 1 ]; then
  PLATFORM="$1"
else
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) os=windows ;;
    *) echo "unsupported system: $(uname -s)" >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) echo "unsupported machine: $(uname -m)" >&2; exit 1 ;;
  esac
  PLATFORM="$os-$arch"
fi
EXT=""
case "$PLATFORM" in windows-*) EXT=".exe" ;; esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# GitHub's release download links sometimes time out while its API still
# serves the same files, so fall back to the API. Checksums guard both ways.
RELEASE="$(mktemp)"
trap 'rm -f "$RELEASE"' EXIT
download() { # release file name, destination
  curl -fsSL --retry 2 --retry-all-errors --retry-delay 3 "$BASE/$1" -o "$2" && return
  echo "direct download of $1 failed; trying the GitHub API" >&2
  [ -s "$RELEASE" ] || curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 "$API" -o "$RELEASE"
  # Each asset's "url" comes before its "name".
  id=$(awk -v f="$1" '
    /"url": *"https:[^"]*\/releases\/assets\/[0-9]+"/ { u = $0; sub(/.*\/assets\//, "", u); sub(/".*/, "", u); id = u }
    /"name": *"/ { n = $0; sub(/^[^:]*: *"/, "", n); sub(/".*/, "", n); if (n == f && id != "") { print id; exit } }
  ' "$RELEASE")
  [ -n "$id" ] || { echo "$1 is not an asset of $VERSION" >&2; return 1; }
  curl -fsSL --retry 3 --retry-all-errors --retry-delay 3 -H "Accept: application/octet-stream" \
    "https://api.github.com/repos/jt-wang/tailpocket/releases/assets/$id" -o "$2"
}

mkdir -p "$DIR/bin" "$DIR/pkg"
SUMS="$DIR/SHA256SUMS"
download SHA256SUMS "$SUMS"

fetch() { # release file name, destination
  want=$(awk -v f="$1" '{ name = $2; sub(/^\*/, "", name); if (name == f) print $1 }' "$SUMS")
  [ -n "$want" ] || { echo "$1 is not listed in SHA256SUMS of $VERSION" >&2; exit 1; }
  if [ -f "$2" ] && [ "$(sha256 "$2")" = "$want" ]; then echo "ok (cached)  $1"; return; fi
  download "$1" "$2.part"
  got=$(sha256 "$2.part")
  if [ "$got" != "$want" ]; then
    rm -f "$2.part"
    echo "SHA-256 mismatch for $1: expected $want, got $got" >&2
    exit 1
  fi
  mv "$2.part" "$2"
  echo "ok           $1"
}

if [ "$PLATFORM" != mobile ]; then
  for tool in tailpocket-host tailpocket-phone; do
    fetch "$tool-$PLATFORM$EXT" "$DIR/bin/$tool-$PLATFORM$EXT"
    chmod +x "$DIR/bin/$tool-$PLATFORM$EXT"
  done
fi
fetch react-native-tailpocket.tgz "$DIR/pkg/react-native-tailpocket.tgz"
fetch tailpocket-client.tgz "$DIR/pkg/tailpocket-client.tgz"
# The Android library behind react-native-tailpocket, as a local Maven repository.
fetch tailpocket-android-m2.zip "$DIR/pkg/tailpocket-android-m2.zip"
if [ ! -d "$DIR/pkg/m2" ]; then
  (cd "$DIR/pkg" && unzip -q tailpocket-android-m2.zip)
fi
echo "$VERSION" > "$DIR/VERSION"
echo "TailPocket $VERSION for $PLATFORM in vendor/tailpocket"
