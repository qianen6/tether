# Source from the repository root before a release build:
#
#   . scripts/release-rustflags.sh && cargo build --release -p tether
#
# Rust records where each dependency's source sits (`~/.cargo/registry/...`,
# with the user name) for panic messages; these become `/cargo`, `/rustup`
# and `.` so a release carries none of the build machine's paths.

_root="$(pwd)"
_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
_rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"

# rustc sees Windows paths in Windows form.
_native() {
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) cygpath -w "$1" ;;
    *) printf '%s' "$1" ;;
  esac
}

# One flag per field (0x1f), so paths with spaces stay whole.
_sep="$(printf '\037')"
CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$(_native "$_cargo_home")=/cargo"
CARGO_ENCODED_RUSTFLAGS="$CARGO_ENCODED_RUSTFLAGS$_sep--remap-path-prefix=$(_native "$_rustup_home")=/rustup"
CARGO_ENCODED_RUSTFLAGS="$CARGO_ENCODED_RUSTFLAGS$_sep--remap-path-prefix=$(_native "$_root")=."
export CARGO_ENCODED_RUSTFLAGS
unset _root _cargo_home _rustup_home _sep
unset -f _native
