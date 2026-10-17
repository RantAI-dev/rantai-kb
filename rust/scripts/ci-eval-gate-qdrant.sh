#!/usr/bin/env bash
# Continuous integration retrieval quality and scope isolation gate for Qdrant.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export KB_CHUNK_STORE="qdrant"
exec "$SCRIPT_DIR/ci-eval-gate.sh" "$@"
