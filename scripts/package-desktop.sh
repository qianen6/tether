#!/bin/sh
# Builds the desktop installer without this computer's paths inside: Rust
# records where each dependency's source sits (`~/.cargo/registry/...`, with
# the user name) for panic messages; these become `/cargo`, `/rustup` and `.`.
#
#   scripts/package-desktop.sh            # same arguments as `tauri build`
#
# Set CARGO_TARGET_DIR to build elsewhere (e.g. while the installed app holds
# target/release).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

# rustc sees Windows paths in Windows form.
native() {
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) cygpath -w "$1" ;;
    *) printf '%s' "$1" ;;
  esac
}

# One flag per field (0x1f), so paths with spaces stay whole.
sep="$(printf '\037')"
flags="--remap-path-prefix=$(native "$cargo_home")=/cargo"
flags="$flags$sep--remap-path-prefix=$(native "$rustup_home")=/rustup"
flags="$flags$sep--remap-path-prefix=$(native "$ROOT")=."
CARGO_ENCODED_RUSTFLAGS="$flags"
export CARGO_ENCODED_RUSTFLAGS

cd "$ROOT/apps/desktop"
pnpm tauri build "$@"
