#!/usr/bin/env bash
# One command for a complete rantai-kb (Rust) stack, per deployment tier.
#
#   rust/scripts/kb-dev.sh up lite-en          # or lite | desk | studio | station | cloud
#   rust/scripts/kb-dev.sh key my-tenant       # prints an API key
#   rust/scripts/kb-dev.sh status              # GET /health
#   rust/scripts/kb-dev.sh logs                # follow everything for this tier
#   rust/scripts/kb-dev.sh down                # stop (down -v also wipes data)
#
# It runs rust/deploy/compose.yaml under the compose project `rantai-kb-dev`.
# Everything it creates belongs to that project and nothing else is touched:
# the only host port published is kb-server's (KB_PORT, default 8080).
#
# The tier is remembered in rust/deploy/.tier, so `down`, `logs`, `ps`, `key`
# and `status` work without repeating it. Settings live in rust/deploy/.env
# (copy .env.example); shell exports win over that file.
#
# `up` builds the image when it is missing, and otherwise only when asked
# (`up <tier> --build`).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "$SCRIPT_DIR/../deploy" && pwd)"
COMPOSE_FILE="$DEPLOY_DIR/compose.yaml"
ENV_FILE="$DEPLOY_DIR/.env"
TIER_FILE="$DEPLOY_DIR/.tier"
PROJECT="${KB_DEV_PROJECT:-rantai-kb-dev}"
IMAGE="rantai-kb-rust:${KB_IMAGE_TAG:-dev}"
TIERS=(lite lite-en desk studio station cloud)
CMD=""

usage() {
  cat <<'EOF'
usage: kb-dev.sh <command> [args]

  up <tier> [--build]   start the tier (lite | lite-en | desk | studio | station | cloud)
  down [-v|--volumes]   stop the last-started tier; -v also removes its volumes
  logs [--no-follow] [service...]
                        follow this tier's logs (default: last 200 lines)
  ps                    show this tier's containers
  key <tenant> [name]   create and print an API key for a tenant
  status                GET /health on the running tier

Environment:
  KB_PORT               host port for kb-server (default 8080, also .env)
  KB_DEV_OVERRIDE       extra compose file layered on top, e.g. local tweaks
  KB_WAIT_SECONDS       how long `up` waits for /health (default 900)
  KB_API_URL            base URL for status (default http://127.0.0.1:$KB_PORT)
  OPENROUTER_API_KEY    required by `up cloud` (shell or rust/deploy/.env)

Docs: rust/deploy/README.md
EOF
}

die() {
  printf 'kb-dev: %s\n' "$*" >&2
  exit 1
}

note() {
  printf '%s\n' "$*"
}

require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed"
  docker compose version >/dev/null 2>&1 ||
    die "docker compose v2 is required (the 'docker compose' plugin)"
  [ -f "$COMPOSE_FILE" ] || die "missing $COMPOSE_FILE"
}

valid_tier() {
  local name
  for name in "${TIERS[@]}"; do
    if [ "$name" = "$1" ]; then
      return 0
    fi
  done
  return 1
}

# A value from rust/deploy/.env, without sourcing it (values here are plain
# KEY=value lines; quotes, trailing comments and spaces are stripped).
env_file_value() {
  local name="$1" line value
  [ -f "$ENV_FILE" ] || return 1
  line="$(grep -E "^[[:space:]]*${name}=" "$ENV_FILE" | tail -n 1)" || return 1
  value="${line#*=}"
  value="${value%%#*}"
  value="${value//[[:space:]]/}"
  value="${value%\"}"
  value="${value#\"}"
  value="${value%\'}"
  value="${value#\'}"
  printf '%s' "$value"
}

api_port() {
  local port="${KB_PORT:-}"
  if [ -z "$port" ]; then
    port="$(env_file_value KB_PORT || true)"
  fi
  printf '%s' "${port:-8080}"
}

api_url() {
  printf '%s' "${KB_API_URL:-http://127.0.0.1:$(api_port)}"
}

has_openrouter_key() {
  local value="${OPENROUTER_API_KEY:-}"
  if [ -z "$value" ]; then
    value="$(env_file_value OPENROUTER_API_KEY || true)"
  fi
  [ -n "$value" ]
}

compose() {
  local files=(-f "$COMPOSE_FILE")
  if [ -n "${KB_DEV_OVERRIDE:-}" ]; then
    files+=(-f "$KB_DEV_OVERRIDE")
  fi
  docker compose "${files[@]}" -p "$PROJECT" "$@"
}

compose_for() {
  local tier="$1"
  shift
  local files=(-f "$COMPOSE_FILE")
  if [ -n "${KB_DEV_OVERRIDE:-}" ]; then
    files+=(-f "$KB_DEV_OVERRIDE")
  fi
  docker compose "${files[@]}" -p "$PROJECT" --profile "$tier" "$@"
}

last_tier() {
  if [ -s "$TIER_FILE" ]; then
    head -n 1 "$TIER_FILE"
  fi
}

# The tier for a command that does not take one: an explicit argument, else the
# remembered one. Writes the explicit argument back to the memory file.
resolve_tier() {
  local tier="${1:-}"
  if [ -z "$tier" ]; then
    tier="$(last_tier)"
  fi
  if [ -z "$tier" ]; then
    die "no tier recorded in $TIER_FILE — pass one: kb-dev.sh $CMD <tier>"
  fi
  valid_tier "$tier" || die "unknown tier '$tier' (choose: ${TIERS[*]})"
  printf '%s\n' "$tier" >"$TIER_FILE"
  printf '%s' "$tier"
}

# A second kb-server for another tier would collide on KB_PORT, and two tiers on
# one set of volumes would mix embedding dimensions. Refuse early, by name.
check_no_other_tier() {
  local tier="$1" service
  local -a running
  mapfile -t running < <(docker ps \
    --filter "label=com.docker.compose.project=$PROJECT" \
    --format '{{.Label "com.docker.compose.service"}}' 2>/dev/null |
    grep -E '^kb-' || true)
  for service in "${running[@]}"; do
    if [ "$service" != "kb-$tier" ]; then
      die "the '$service' stack is already running; run 'kb-dev.sh down' first (or 'down -v' to also wipe data)"
    fi
  done
}

wait_healthy() {
  local tier="$1" url seconds elapsed
  url="$(api_url)"
  seconds="${KB_WAIT_SECONDS:-900}"
  note "waiting for $url/health (first start downloads the model; up to ${seconds}s)"
  elapsed=0
  while [ "$elapsed" -lt "$seconds" ]; do
    if curl -fsS -m 3 "$url/health" >/dev/null 2>&1; then
      note "kb-server is healthy at $url"
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done
  printf '\nkb-server did not become healthy at %s\n' "$url" >&2
  compose_for "$tier" ps >&2 || true
  compose_for "$tier" logs --tail=40 "kb-$tier" >&2 || true
  die "health check timed out (see the logs above)"
}

cmd_up() {
  local tier="" build=0 arg
  for arg in "$@"; do
    case "$arg" in
      --build) build=1 ;;
      -*) die "up: unknown option '$arg'" ;;
      *)
        if [ -z "$tier" ]; then
          tier="$arg"
        else
          die "up: unexpected argument '$arg'"
        fi
        ;;
    esac
  done
  [ -n "$tier" ] || die "usage: kb-dev.sh up <tier> [--build]"
  valid_tier "$tier" || die "unknown tier '$tier' (choose: ${TIERS[*]})"

  if [ "$tier" = "cloud" ] && ! has_openrouter_key; then
    die "KB_TIER=cloud needs OPENROUTER_API_KEY — put it in $ENV_FILE
       (copy .env.example first) or export it. Cloud embeds and reranks at
       OpenRouter, so it cannot run without a paid key."
  fi

  check_no_other_tier "$tier"
  printf '%s\n' "$tier" >"$TIER_FILE"

  if [ "$build" = "0" ] && ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    note "image $IMAGE not found; building it (CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2})"
    build=1
  fi
  if [ "$build" = "1" ]; then
    compose_for "$tier" build "kb-$tier"
  fi
  compose_for "$tier" up -d --no-build

  wait_healthy "$tier"
  note ""
  note "rantai-kb ($tier) is up."
  note "  API      $(api_url)"
  note "  health   $(api_url)/health"
  note "  API key  rust/scripts/kb-dev.sh key my-tenant"
  note "  upload   curl -H \"Authorization: Bearer <key>\" -F file=@doc.pdf $(api_url)/v1/documents"
  note "  logs     rust/scripts/kb-dev.sh logs"
  note "  stop     rust/scripts/kb-dev.sh down        (down -v also wipes data)"
}

cmd_down() {
  local volumes=0 arg tier=""
  for arg in "$@"; do
    case "$arg" in
      -v | --volumes) volumes=1 ;;
      -*) die "down: unknown option '$arg'" ;;
      *) tier="$arg" ;;
    esac
  done
  tier="$(resolve_tier "$tier")"
  if [ "$volumes" = "1" ]; then
    note "removing the $tier stack including its volumes (this deletes data)"
    compose_for "$tier" down --volumes
  else
    compose_for "$tier" down
    note "stopped $tier; data volumes kept (use 'kb-dev.sh down -v' to delete them)"
  fi
}

cmd_logs() {
  local follow=1 arg tier services=()
  for arg in "$@"; do
    case "$arg" in
      -f | --follow) follow=1 ;;
      --no-follow) follow=0 ;;
      -*) die "logs: unknown option '$arg'" ;;
      *) services+=("$arg") ;;
    esac
  done
  tier="$(resolve_tier "")"
  if [ "${#services[@]}" -gt 0 ]; then
    if [ "$follow" = "1" ]; then
      compose_for "$tier" logs --tail="${KB_LOG_TAIL:-200}" --follow "${services[@]}"
    else
      compose_for "$tier" logs --tail="${KB_LOG_TAIL:-200}" "${services[@]}"
    fi
  else
    if [ "$follow" = "1" ]; then
      compose_for "$tier" logs --tail="${KB_LOG_TAIL:-200}" --follow
    else
      compose_for "$tier" logs --tail="${KB_LOG_TAIL:-200}"
    fi
  fi
}

cmd_ps() {
  local tier
  tier="$(resolve_tier "")"
  note "profile: $tier"
  compose_for "$tier" ps
}

cmd_key() {
  local tenant="${1:-}" name="${2:-}" tier
  [ -n "$tenant" ] || die "usage: kb-dev.sh key <tenant> [name]"
  tier="$(resolve_tier "")"
  if [ -n "$name" ]; then
    compose_for "$tier" exec -T "kb-$tier" kb-server create-api-key "$tenant" "$name"
  else
    compose_for "$tier" exec -T "kb-$tier" kb-server create-api-key "$tenant"
  fi
}

cmd_status() {
  local tier url
  tier="$(resolve_tier "")"
  url="$(api_url)"
  if curl -fsS -m 5 "$url/health"; then
    printf '\n'
  else
    printf 'kb-server (%s) at %s is not healthy\n' "$tier" "$url" >&2
    compose_for "$tier" ps >&2
    exit 1
  fi
}

main() {
  CMD="${1:-}"
  if [ "$#" -gt 0 ]; then
    shift
  fi
  case "$CMD" in
    up) require_docker; cmd_up "$@" ;;
    down) require_docker; cmd_down "$@" ;;
    logs) require_docker; cmd_logs "$@" ;;
    ps) require_docker; cmd_ps "$@" ;;
    key) require_docker; cmd_key "$@" ;;
    status) require_docker; cmd_status "$@" ;;
    "" | -h | --help | help) usage ;;
    *) die "unknown command '$CMD' (try: kb-dev.sh --help)" ;;
  esac
}

main "$@"
