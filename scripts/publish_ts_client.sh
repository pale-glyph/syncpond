#!/usr/bin/env bash
set -euo pipefail

# Publish syncpond-client package to private Verdaccio registry.
# Usage:
#   ./scripts/publish_ts_client.sh [--registry URL] [--tag TAG] [--dry-run] [--help]
#
# Default options:
#   registry: https://npm.lab-2.paleglyph.com/
#   tag: latest

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="${SCRIPT_DIR%/*}"
CLIENT_DIR="$REPO_ROOT/syncpond-client"

REGISTRY="https://npm.lab-2.paleglyph.com/"
TAG="latest"
DRY_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --registry)
      REGISTRY="$2"; shift 2;;
    --tag)
      TAG="$2"; shift 2;;
    --dry-run)
      DRY_RUN=true; shift;;
    -h|--help)
      cat <<EOF
Usage: $0 [--registry URL] [--tag TAG] [--dry-run] [--help]

  --registry URL    Verdaccio registry URL (default: https://npm.lab-2.paleglyph.com/)
  --tag TAG         npm dist-tag to publish (default: latest)
  --dry-run         do not publish, only build and show command
  -h, --help        show this help
EOF
      exit 0;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1;;
  esac
done

cd "$CLIENT_DIR"

echo "[*] Building syncpond-client in $CLIENT_DIR"
npm ci
npm run build

echo "[*] Setting npm registry: $REGISTRY"
npm config set registry "$REGISTRY"

if [[ "$DRY_RUN" == true ]]; then
  echo "[Dry run] npm publish --tag $TAG --registry $REGISTRY"
  npm publish --tag "$TAG" --registry "$REGISTRY" --dry-run
  echo "[Dry run] complete"
  exit 0
fi

echo "[*] Publishing @syncpond/client@$TAG to $REGISTRY"
npm publish --tag "$TAG" --registry "$REGISTRY"

echo "[+] Published @syncpond/client@$TAG to $REGISTRY"
