#!/usr/bin/env bash
# Stop the kb-smoke stack started by stack-up.sh.
#
# Only the two pids in $KB_STACK_DIR and the `$KB_STACK_NAME-*` containers
# (default `kb-smoke-*`) are touched; pass the same KB_STACK_DIR and
# KB_STACK_NAME that stack-up.sh was given. Other people's containers on this host are never named here, and a
# missing container is not an error.
set -euo pipefail

STACK_DIR="${KB_STACK_DIR:-/tmp/kb-stack}"
STACK_NAME="${KB_STACK_NAME:-kb-smoke}"
SERVER_PID_FILE="$STACK_DIR/server.pid"
STUB_PID_FILE="$STACK_DIR/stub.pid"

stop_pid_file() {
  local file="$1" name="$2" pid
  if [ -f "$file" ]; then
    pid="$(cat "$file")"
    if kill -0 "$pid" 2>/dev/null; then
      echo "stopping $name (pid $pid)"
      kill "$pid" 2>/dev/null || true
      for _ in {1..20}; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.5
      done
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$file"
  fi
}

stop_pid_file "$SERVER_PID_FILE" "kb-server"
stop_pid_file "$STUB_PID_FILE" "stub embeddings"

tmux kill-session -t "${STACK_NAME}-server" 2>/dev/null || true
tmux kill-session -t "${STACK_NAME}-stub" 2>/dev/null || true

containers=()
for part in pg surreal rustfs tei-embed tei-rerank qdrant; do
  containers+=("${STACK_NAME}-${part}")
done
echo "removing containers ${containers[*]}"
docker rm -f "${containers[@]}" >/dev/null 2>&1 || true

rm -f "$STACK_DIR/stack.env"
echo "${STACK_NAME} stack is down (logs left in $STACK_DIR for inspection)"
