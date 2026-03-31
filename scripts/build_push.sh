#!/usr/bin/env bash
set -euo pipefail

# Build and push the syncpond server Docker image.
# Usage:
#   ./scripts/build-and-push-syncpond-server.sh [--image-name NAME] [--tag TAG] [--push] [--no-push]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="${SCRIPT_DIR%/*}"

IMAGE_NAME="paleglyph/syncpond"
TAG=""
PUSH=true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image-name)
      IMAGE_NAME="$2"; shift 2;;
    --tag)
      TAG="$2"; shift 2;;
    --push)
      PUSH=true; shift;;
    --no-push)
      PUSH=false; shift;;
    -h|--help)
      cat <<EOF
Usage: $0 [--image-name NAME] [--tag TAG] [--push] [--no-push]

  --image-name NAME  Docker image name (default: paleglyph/syncpond)
  --tag TAG         Image tag (default: git describe --tags --always or latest)
  --push            Push the image to registry (default)
  --no-push         Build only, do not push
  -h, --help        Show this help message
EOF
      exit 0;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1;;
  esac
done

if [[ -z "$TAG" ]]; then
  if git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    TAG=$(git -C "$REPO_ROOT" describe --tags --dirty --always 2>/dev/null || true)
  fi
  TAG=${TAG:-latest}
fi

FULL_IMAGE="${IMAGE_NAME}:${TAG}"

echo "[*] Building syncpond server Docker image: $FULL_IMAGE"
cd "$REPO_ROOT/syncpond-server"

docker build --pull -t "$FULL_IMAGE" -f Dockerfile .

if [[ "$PUSH" == true ]]; then
  echo "[*] Pushing Docker image: $FULL_IMAGE"
  docker push "$FULL_IMAGE"
  echo "[+] Successfully pushed $FULL_IMAGE"
else
  echo "[+] Build complete; push skipped for $FULL_IMAGE"
fi
