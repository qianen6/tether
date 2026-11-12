#!/bin/sh
# Safely removes a task worktree made with `git worktree add` + link-shared.sh.
# The links are removed first (only the links, never their targets), because
# deleting a directory tree that contains them could follow them into the main
# checkout's .ref/ and vendor/.
#
#   scripts/remove-worktree.sh ../tether-T04
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <worktree path>" >&2; exit 1; }
WT="$(cd "$1" && pwd)"
MAIN="$(git -C "$WT" worktree list --porcelain | awk 'NR == 1 { sub(/^worktree /, ""); print }')"
[ "$WT" != "$MAIN" ] || { echo "refusing to remove the main checkout" >&2; exit 1; }

for p in .ref vendor/tailpocket/bin vendor/tailpocket/pkg; do
  [ -e "$WT/$p" ] || [ -L "$WT/$p" ] || continue
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
      # Directory.Delete(path, false) on a junction removes the junction only.
      LINK_AT="$(cygpath -w "$WT/$p")" powershell.exe -NoProfile -Command \
        '$i = Get-Item -LiteralPath $env:LINK_AT -Force; if ($i.LinkType) { [IO.Directory]::Delete($env:LINK_AT, $false) } else { throw "not a link: $env:LINK_AT" }' ;;
    *)
      [ -L "$WT/$p" ] || { echo "not a link: $WT/$p" >&2; exit 1; }
      rm "$WT/$p" ;;
  esac
  echo "unlinked     $p"
done

# What is left (node_modules, target) may still hold links, e.g. pnpm's.
# Delete the tree only if every remaining link points inside it.
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    outside=$(WT_DIR="$(cygpath -w "$WT")" powershell.exe -NoProfile -Command \
      '@(Get-ChildItem -LiteralPath $env:WT_DIR -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue |
         Where-Object { $t = [string]$_.Target; $t -and -not $t.StartsWith($env:WT_DIR) }).Count' | tr -d '\r') ;;
  *)
    outside=$(find "$WT" -type l -exec sh -c 'case "$(cd "$(dirname "$1")" && realpath "$(readlink "$1")" 2>/dev/null)" in "$2"*) ;; *) echo x ;; esac' _ {} "$WT" \; | wc -l | tr -d ' ') ;;
esac
if [ "$outside" != "0" ]; then
  echo "refusing to delete $WT: $outside link(s) point outside it; remove them by hand" >&2
  exit 1
fi

rm -rf "$WT"
git -C "$MAIN" worktree prune
echo "removed      $WT"
