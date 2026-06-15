#!/usr/bin/env bash
#
# Fast local Docker dev loop for sandbox-agent.
#
#   scripts/dev-docker.sh base    build the slim base image (release + selected
#                                 agents only; see Dockerfile.clawlabor-base)
#   scripts/dev-docker.sh build   build the fast dev image (debug + binary-swap
#                                 onto the base; builds the base first if missing)
#   scripts/dev-docker.sh up      build, then (re)start the container on :2468
#                                 with the Claude subscription OAuth token
#   scripts/dev-docker.sh down    stop and remove the container
#
# Token: uses $CLAUDE_CODE_OAUTH_TOKEN if set, otherwise reads the Claude Code
# subscription OAuth token from the macOS keychain. Override the platform with
# $PLATFORM (default linux/arm64) on Intel/amd64 machines.
set -euo pipefail

IMAGE="sandbox-clawlabor:devfast"
BASE_IMAGE="sandbox-clawlabor:base"
NAME="sandbox-clawlabor-dev"
PORT="${PORT:-2468}"
PLATFORM="${PLATFORM:-linux/arm64}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

base() {
  DOCKER_BUILDKIT=1 docker build --platform "$PLATFORM" \
    -f "$ROOT/docker/runtime/Dockerfile.clawlabor-base" -t "$BASE_IMAGE" "$ROOT"
}

build() {
  # The devfast image's FROM defaults to $BASE_IMAGE; build it first if absent.
  if ! docker image inspect "$BASE_IMAGE" >/dev/null 2>&1; then
    echo "base image $BASE_IMAGE not found; building it first..."
    base
  fi
  DOCKER_BUILDKIT=1 docker build --platform "$PLATFORM" \
    -f "$ROOT/docker/runtime/Dockerfile.devfast" -t "$IMAGE" "$ROOT"
}

resolve_token() {
  if [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
    printf '%s' "$CLAUDE_CODE_OAUTH_TOKEN"
    return
  fi
  security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['claudeAiOauth']['accessToken'])" 2>/dev/null \
    || true
}

up() {
  build
  local tok
  tok="$(resolve_token)"
  if [ -z "$tok" ]; then
    echo "warning: no Claude OAuth token found (env or keychain); /usage will report no subscription credential" >&2
  fi
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker run -d --name "$NAME" -p "$PORT:2468" \
    ${tok:+-e CLAUDE_CODE_OAUTH_TOKEN="$tok"} \
    "$IMAGE" server --no-token --host 0.0.0.0 --port 2468 >/dev/null
  printf 'waiting for health'
  for _ in $(seq 1 30); do
    if curl -fsS "http://localhost:$PORT/v1/health" >/dev/null 2>&1; then
      echo
      echo "up: http://localhost:$PORT  (Inspector: http://localhost:$PORT/ui/)"
      return 0
    fi
    printf '.'
    sleep 1
  done
  echo
  echo "server did not become healthy; check: docker logs $NAME" >&2
  return 1
}

down() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  echo "stopped $NAME"
}

case "${1:-up}" in
  base) base ;;
  build) build ;;
  up) up ;;
  down) down ;;
  *) echo "usage: $0 {base|build|up|down}" >&2; exit 1 ;;
esac
