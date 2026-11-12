#!/bin/sh
# Builds the desktop installer without this computer's paths inside (see
# release-rustflags.sh).
#
#   scripts/package-desktop.sh            # same arguments as `tauri build`
#
# Set CARGO_TARGET_DIR to build elsewhere (e.g. while the installed app holds
# target/release).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
. scripts/release-rustflags.sh

cd apps/desktop
pnpm tauri build "$@"
