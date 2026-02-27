#!/bin/bash
# Build Docker images for Savfox.
# Usage: ./scripts/docker/build.sh [--push] [--tag TAG]
set -euo pipefail

TAG="${TAG:-latest}"
PUSH=false
REGISTRY="${REGISTRY:-ghcr.io/savfox-ai/savfox}"

while [[ $# -gt 0 ]]; do
    case $1 in
        --push) PUSH=true; shift ;;
        --tag) TAG="$2"; shift 2 ;;
        --registry) REGISTRY="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

IMAGE="${REGISTRY}:${TAG}"

echo "Building Docker image: ${IMAGE}"
docker build \
    -t "${IMAGE}" \
    -f Dockerfile \
    --build-arg BUILDKIT_INLINE_CACHE=1 \
    .

echo "Building sandbox image..."
docker build \
    -t "${REGISTRY}-sandbox:${TAG}" \
    -f Dockerfile.sandbox \
    .

echo ""
echo "Images built:"
echo "  ${IMAGE}"
echo "  ${REGISTRY}-sandbox:${TAG}"

if [[ "$PUSH" == "true" ]]; then
    echo ""
    echo "Pushing images..."
    docker push "${IMAGE}"
    docker push "${REGISTRY}-sandbox:${TAG}"
    echo "Done."
fi
