#!/usr/bin/env bash
set -euo pipefail

APP_NAME="databeam"
IMAGE_NAME="databeam"
PORT="80"

if [[ "${EUID}" -ne 0 ]]; then
  echo "Please run as root (or with sudo)." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

docker build -t "${IMAGE_NAME}" .

docker stop "${APP_NAME}" >/dev/null 2>&1 || true
docker rm "${APP_NAME}" >/dev/null 2>&1 || true

docker run -d --name "${APP_NAME}" -p "${PORT}:8080" "${IMAGE_NAME}"

echo "${APP_NAME} is running at http://localhost:${PORT}"
