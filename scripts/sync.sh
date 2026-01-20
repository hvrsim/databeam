#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 user@host:/remote/path [--port 22]" >&2
  exit 1
}

if [[ $# -lt 1 ]]; then
  usage
fi

DEST="$1"
PORT="22"

if [[ "${2:-}" == "--port" && -n "${3:-}" ]]; then
  PORT="$3"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

rsync -az --delete \
  --exclude ".git/" \
  --filter=":- .gitignore" \
  -e "ssh -p ${PORT}" \
  "${SCRIPT_DIR}/" "${DEST}"

echo "Pushed to ${DEST}"
