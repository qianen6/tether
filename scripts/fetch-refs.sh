#!/bin/sh
# Reference sources for the tasks, read-only, in .ref/ (not committed):
# Boop, Herdr Anywhere, TailPocket (pinned tag), the ACP spec, and the JSON
# schema of the installed Codex app-server.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="$ROOT/.ref"
mkdir -p "$REF"

clone() { # url, dir, [tag]
  if [ -d "$REF/$2/.git" ]; then echo "ok (cached)  $2"; return; fi
  if [ $# -ge 3 ]; then
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$3" "$1" "$REF/$2"
  else
    git clone -q --depth 1 "$1" "$REF/$2"
  fi
  echo "ok           $2 @ $(git -C "$REF/$2" rev-parse --short HEAD)"
}

clone https://github.com/jt-wang/boop.git boop
clone https://github.com/jt-wang/herdr-anywhere.git herdr-anywhere
clone https://github.com/jt-wang/tailpocket.git tailpocket "${TAILPOCKET_VERSION:-v0.2.2}"
clone https://github.com/agentclientprotocol/agent-client-protocol.git agent-client-protocol

if command -v codex >/dev/null 2>&1; then
  rm -rf "$REF/codex-schema"
  codex app-server generate-json-schema --out "$REF/codex-schema"
  codex --version > "$REF/codex-schema/VERSION"
  echo "ok           codex-schema ($(cat "$REF/codex-schema/VERSION"))"
else
  echo "codex is not installed: .ref/codex-schema skipped (T04 needs it)" >&2
fi
