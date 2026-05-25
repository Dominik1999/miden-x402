#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# bench-local.sh — Run the batch-settlement e2e benchmark locally on testnet.
#
# This runs the proven batch_settlement_e2e test with timing output.
# For multi-server AWS benchmarks, see benchmark-aws.sh.
#
# Usage:
#   ./scripts/bench-local.sh              # default: 5 vouchers
#   ./scripts/bench-local.sh --build      # force rebuild first
###############################################################################

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUILD=false
for arg in "$@"; do
  case "$arg" in
    --build) BUILD=true ;;
    --help|-h) echo "Usage: $0 [--build]"; exit 0 ;;
  esac
done

cd "$REPO_ROOT"

if [ "$BUILD" = true ]; then
  echo "=== Building in release mode ==="
  cargo test --release -p agent-debit-note --test batch_settlement_e2e --no-run 2>&1 | tail -3
fi

echo ""
echo "=== Running batch-settlement e2e benchmark on testnet ==="
echo "    Miden RPC: https://rpc.testnet.miden.io"
echo "    Flow: setup → 5 cumulative vouchers (off-chain) → settle → merchant consumes P2ID"
echo ""

START=$(date +%s)

RUST_LOG=info cargo test --release -p agent-debit-note \
  --test batch_settlement_e2e -- --ignored --nocapture 2>&1 | \
  while IFS= read -r line; do
    echo "$line"
  done

END=$(date +%s)
ELAPSED=$((END - START))

echo ""
echo "=== BENCHMARK COMPLETE ==="
echo "    Total time: ${ELAPSED}s"
echo ""
