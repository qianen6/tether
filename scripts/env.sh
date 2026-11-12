# Source this in Git Bash / sh to use the project's Node (conda env "tether",
# Node 22 + pnpm) without touching the system Node:
#
#   . scripts/env.sh
#
# In PowerShell or cmd, `conda activate tether` does the same.
# Create the env once with:
#   conda create -n tether -c conda-forge "nodejs=22" pnpm

_tether_env="${TETHER_NODE_ENV:-$(conda env list 2>/dev/null | awk '$1 == "tether" { print $NF }')}"
if [ -z "$_tether_env" ]; then
  echo "conda env 'tether' not found; create it: conda create -n tether -c conda-forge \"nodejs=22\" pnpm" >&2
else
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
      _tether_env="$(cygpath -u "$_tether_env")"
      PATH="$_tether_env:$_tether_env/Library/bin:$_tether_env/Scripts:$PATH"
      # conda ships pnpm as pnpm.bat, which sh does not find by the bare name.
      pnpm() { pnpm.bat "$@"; }
      ;;
    *) PATH="$_tether_env/bin:$PATH" ;;
  esac
  export PATH
  echo "node $(node --version) from conda env tether"
fi
unset _tether_env
