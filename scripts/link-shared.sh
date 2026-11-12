#!/bin/sh
# Run once in a new git worktree (docs/PLAN.md): links the downloaded,
# uncommitted directories from the main checkout and installs JS packages
# from pnpm's local store, so the task can build offline.
set -eu

HERE="$(cd "$(dirname "$0")/.." && pwd)"
MAIN="$(git -C "$HERE" worktree list --porcelain | awk 'NR == 1 { sub(/^worktree /, ""); print }')"
if [ "$HERE" = "$MAIN" ]; then echo "this is the main checkout; nothing to link"; exit 0; fi

link() { # path relative to the repository root
  [ -e "$MAIN/$1" ] || { echo "missing in main checkout: $1 (run scripts/fetch-*.sh there)" >&2; exit 1; }
  [ -e "$HERE/$1" ] && { echo "ok (exists)  $1"; return; }
  mkdir -p "$(dirname "$HERE/$1")"
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
      # A junction needs no admin rights; PowerShell (unlike `cmd /c mklink`)
      # passes non-ASCII paths through intact.
      LINK_AT="$(cygpath -w "$HERE/$1")" LINK_TO="$(cygpath -w "$MAIN/$1")" \
        powershell.exe -NoProfile -Command 'New-Item -ItemType Junction -Path $env:LINK_AT -Target $env:LINK_TO | Out-Null' ;;
    *) ln -s "$MAIN/$1" "$HERE/$1" ;;
  esac
  echo "ok           $1"
}

link .ref
link vendor/tailpocket/bin
link vendor/tailpocket/pkg

# Use the project Node (conda env "tether") when it exists.
. "$HERE/scripts/env.sh" || true
(cd "$HERE" && pnpm install --offline --frozen-lockfile)
echo "worktree ready: cargo build --offline and pnpm work without network"
