#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# benchmark-aws.sh
#
# Multi-server benchmark for the miden-x402 batch-settlement (ADN) flow.
#
# Architecture:
#   Agent server       — creates ADN note, signs cumulative vouchers
#   Merchant server    — runs HTTP paywall, verifies vouchers, calls /settle
#   Facilitator server — runs /verify and /settle endpoints, consumes ADN notes
#
# Supports three modes:
#   --local            all three roles on localhost (no SSH)
#   default            three remote hosts via SSH
#   --dry-run          print commands without executing
###############################################################################

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ─── Defaults ────────────────────────────────────────────────────────────────

AGENT_HOST="localhost"
MERCHANT_HOST="localhost"
FACILITATOR_HOST="localhost"
NUM_PAYMENTS=50
AMOUNT_PER_PAYMENT=100
LOCAL_MODE=false
DRY_RUN=false
SSH_USER="${BENCH_SSH_USER:-ubuntu}"
SSH_KEY="${BENCH_SSH_KEY:-}"
BRANCH="${BENCH_BRANCH:-oz-guardian-latest-flow}"
REPO_URL="${BENCH_REPO_URL:-https://github.com/Digine-Labs/miden-x402.git}"
MIDEN_RPC="${MIDEN_RPC:-https://rpc.testnet.miden.io}"
FACILITATOR_PORT=7002
MERCHANT_PORT=7001
RESULTS_DIR="${REPO_ROOT}/bench-results"
RUN_ID="batch-$(date +%Y%m%d-%H%M%S)"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"

# ─── Usage ───────────────────────────────────────────────────────────────────

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Multi-server benchmark for the miden-x402 ADN batch-settlement flow.

Options:
  --agent-host HOST          Agent server hostname/IP       (default: localhost)
  --merchant-host HOST       Merchant server hostname/IP    (default: localhost)
  --facilitator-host HOST    Facilitator server hostname/IP (default: localhost)
  --payments N               Number of voucher payments     (default: 50)
  --amount-per-payment N     Amount per payment in tokens   (default: 100)
  --local                    Run all roles on localhost, no SSH
  --dry-run                  Print what would happen without executing
  --ssh-user USER            SSH user for remote hosts      (default: ubuntu)
  --ssh-key PATH             Path to SSH private key        (required for remote)
  --branch BRANCH            Git branch to clone/pull       (default: oz-guardian-latest-flow)
  --miden-rpc URL            Miden testnet RPC URL          (default: https://rpc.testnet.miden.io)
  --results-dir DIR          Directory for benchmark output (default: ./bench-results)
  -h, --help                 Show this help message

Environment variables:
  BENCH_SSH_USER             Alternative to --ssh-user
  BENCH_SSH_KEY              Alternative to --ssh-key
  BENCH_BRANCH               Alternative to --branch
  BENCH_REPO_URL             Alternative to --repo-url
  MIDEN_RPC                  Alternative to --miden-rpc

Examples:
  # Local mode (simplest, good for development):
  $(basename "$0") --local --payments 20

  # Three separate AWS servers:
  $(basename "$0") \\
    --agent-host 54.1.2.3 \\
    --merchant-host 54.4.5.6 \\
    --facilitator-host 54.7.8.9 \\
    --ssh-key ~/.ssh/miden-bench.pem \\
    --payments 100

  # Dry run to preview commands:
  $(basename "$0") --local --payments 10 --dry-run
EOF
  exit 0
}

# ─── Parse arguments ─────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case "$1" in
    --agent-host)        AGENT_HOST="$2";        shift 2 ;;
    --merchant-host)     MERCHANT_HOST="$2";     shift 2 ;;
    --facilitator-host)  FACILITATOR_HOST="$2";  shift 2 ;;
    --payments)          NUM_PAYMENTS="$2";       shift 2 ;;
    --amount-per-payment) AMOUNT_PER_PAYMENT="$2"; shift 2 ;;
    --local)             LOCAL_MODE=true;         shift   ;;
    --dry-run)           DRY_RUN=true;           shift   ;;
    --ssh-user)          SSH_USER="$2";          shift 2 ;;
    --ssh-key)           SSH_KEY="$2";           shift 2 ;;
    --branch)            BRANCH="$2";            shift 2 ;;
    --miden-rpc)         MIDEN_RPC="$2";         shift 2 ;;
    --results-dir)       RESULTS_DIR="$2";       shift 2 ;;
    -h|--help)           usage ;;
    *)
      echo "Unknown option: $1" >&2
      echo "Run with --help for usage." >&2
      exit 1
      ;;
  esac
done

# ─── Helpers ─────────────────────────────────────────────────────────────────

log()  { echo "[$(date '+%H:%M:%S')] $*"; }
err()  { echo "[$(date '+%H:%M:%S')] ERROR: $*" >&2; }
warn() { echo "[$(date '+%H:%M:%S')] WARN:  $*" >&2; }

# Nanosecond timestamp (falls back to microsecond on macOS)
now_ns() {
  if date +%s%N | grep -qv 'N$'; then
    date +%s%N
  else
    # macOS: use python for nanosecond-ish precision
    python3 -c 'import time; print(int(time.time() * 1e9))'
  fi
}

elapsed_ms() {
  local start_ns="$1"
  local end_ns
  end_ns=$(now_ns)
  echo $(( (end_ns - start_ns) / 1000000 ))
}

# Execute on a host: locally or via SSH
run_on() {
  local host="$1"; shift
  if [[ "$DRY_RUN" == true ]]; then
    if [[ "$LOCAL_MODE" == true || "$host" == "localhost" ]]; then
      echo "  [dry-run] bash -c '$*'"
    else
      echo "  [dry-run] ssh ${SSH_USER}@${host} '$*'"
    fi
    return 0
  fi
  if [[ "$LOCAL_MODE" == true || "$host" == "localhost" ]]; then
    bash -c "$*"
  else
    local key_opt=""
    if [[ -n "$SSH_KEY" ]]; then
      key_opt="-i $SSH_KEY"
    fi
    # shellcheck disable=SC2086
    ssh $key_opt $SSH_OPTS "${SSH_USER}@${host}" "$@"
  fi
}

# Execute on a host and prefix output
run_on_prefixed() {
  local host="$1"
  local prefix="$2"
  shift 2
  run_on "$host" "$@" 2>&1 | sed "s/^/  [${prefix}] /"
}

# Start a background process on a host
start_bg() {
  local host="$1"; shift
  local logfile="$1"; shift
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] nohup $* > $logfile 2>&1 &"
    return 0
  fi
  if [[ "$LOCAL_MODE" == true || "$host" == "localhost" ]]; then
    nohup bash -c "$*" > "$logfile" 2>&1 &
    echo $!
  else
    local key_opt=""
    if [[ -n "$SSH_KEY" ]]; then
      key_opt="-i $SSH_KEY"
    fi
    # shellcheck disable=SC2086
    ssh $key_opt $SSH_OPTS "${SSH_USER}@${host}" \
      "nohup bash -lc '$*' > $logfile 2>&1 & echo \$!"
  fi
}

# Wait for an HTTP endpoint to respond
wait_for_http() {
  local url="$1"
  local label="$2"
  local max_attempts="${3:-60}"
  log "Waiting for ${label} at ${url}..."
  for i in $(seq 1 "$max_attempts"); do
    if curl -sf --max-time 5 "$url" >/dev/null 2>&1; then
      log "${label} is ready (attempt ${i})"
      return 0
    fi
    sleep 3
  done
  err "${label} did not become ready after ${max_attempts} attempts"
  return 1
}

# Collect a remote file to local results dir
collect_file() {
  local host="$1"
  local remote_path="$2"
  local local_name="$3"
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] collect ${host}:${remote_path} -> ${RESULTS_DIR}/${RUN_ID}/${local_name}"
    return 0
  fi
  local dest="${RESULTS_DIR}/${RUN_ID}/${local_name}"
  mkdir -p "$(dirname "$dest")"
  if [[ "$LOCAL_MODE" == true || "$host" == "localhost" ]]; then
    cp "$remote_path" "$dest" 2>/dev/null || true
  else
    local key_opt=""
    if [[ -n "$SSH_KEY" ]]; then
      key_opt="-i $SSH_KEY"
    fi
    # shellcheck disable=SC2086
    scp $key_opt $SSH_OPTS "${SSH_USER}@${host}:${remote_path}" "$dest" 2>/dev/null || true
  fi
}

# Track PIDs for cleanup
FACILITATOR_PID=""
MERCHANT_PID=""

cleanup() {
  local exit_code=$?
  echo ""
  log "=== Cleanup ==="

  if [[ -n "$FACILITATOR_PID" ]]; then
    log "Stopping facilitator (PID ${FACILITATOR_PID})..."
    if [[ "$LOCAL_MODE" == true ]]; then
      kill "$FACILITATOR_PID" 2>/dev/null || true
    else
      run_on "$FACILITATOR_HOST" "kill $FACILITATOR_PID" 2>/dev/null || true
    fi
  fi

  if [[ -n "$MERCHANT_PID" ]]; then
    log "Stopping merchant (PID ${MERCHANT_PID})..."
    if [[ "$LOCAL_MODE" == true ]]; then
      kill "$MERCHANT_PID" 2>/dev/null || true
    else
      run_on "$MERCHANT_HOST" "kill $MERCHANT_PID" 2>/dev/null || true
    fi
  fi

  # Collect logs
  if [[ "$DRY_RUN" != true ]]; then
    log "Collecting logs..."
    if [[ "$LOCAL_MODE" == true ]]; then
      collect_file localhost "/tmp/miden-bench-facilitator-${RUN_ID}.log" "facilitator.log"
      collect_file localhost "/tmp/miden-bench-merchant-${RUN_ID}.log" "merchant.log"
    else
      collect_file "$FACILITATOR_HOST" "~/miden-x402/facilitator.log" "facilitator.log"
      collect_file "$MERCHANT_HOST" "~/miden-x402/merchant.log" "merchant.log"
    fi
  fi

  log "Cleanup complete (exit code: ${exit_code})"
}

trap cleanup EXIT

# ─── Prerequisites check ────────────────────────────────────────────────────

log "Checking prerequisites..."

MISSING_TOOLS=()
for tool in curl jq git; do
  if ! command -v "$tool" &>/dev/null; then
    MISSING_TOOLS+=("$tool")
  fi
done

if [[ "$LOCAL_MODE" != true ]]; then
  if ! command -v ssh &>/dev/null; then
    MISSING_TOOLS+=("ssh")
  fi
  if [[ -z "$SSH_KEY" ]]; then
    err "--ssh-key is required for remote mode (or use --local)"
    exit 1
  fi
  if [[ ! -f "$SSH_KEY" ]]; then
    err "SSH key not found: $SSH_KEY"
    exit 1
  fi
fi

if [[ "$LOCAL_MODE" == true ]]; then
  if ! command -v cargo &>/dev/null; then
    MISSING_TOOLS+=("cargo")
  fi
fi

if [[ ${#MISSING_TOOLS[@]} -gt 0 ]]; then
  err "Missing required tools: ${MISSING_TOOLS[*]}"
  err "Install them and try again."
  exit 1
fi

log "All prerequisites satisfied."

###############################################################################
# Configuration summary
###############################################################################

echo ""
log "============================================"
log "  miden-x402 Batch-Settlement Benchmark"
log "  Run ID: ${RUN_ID}"
log "============================================"
echo ""
log "  Mode:             $(if [[ "$LOCAL_MODE" == true ]]; then echo "LOCAL"; else echo "REMOTE (SSH)"; fi)"
log "  Agent host:       ${AGENT_HOST}"
log "  Merchant host:    ${MERCHANT_HOST}"
log "  Facilitator host: ${FACILITATOR_HOST}"
log "  Payments:         ${NUM_PAYMENTS}"
log "  Amount/payment:   ${AMOUNT_PER_PAYMENT}"
log "  Miden RPC:        ${MIDEN_RPC}"
log "  Branch:           ${BRANCH}"
log "  Results dir:      ${RESULTS_DIR}/${RUN_ID}"
if [[ "$DRY_RUN" == true ]]; then
  log "  *** DRY RUN MODE ***"
fi
echo ""

mkdir -p "${RESULTS_DIR}/${RUN_ID}"

###############################################################################
# Phase 1: Setup — clone/pull and build on each server
###############################################################################

T_SETUP_START=$(now_ns)
log "Phase 1: Setup — build release binaries on all hosts"
echo ""

if [[ "$LOCAL_MODE" == true ]]; then
  # Local mode: just build in the current repo
  log "Building release binaries locally..."
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] cargo build --release -p x402-facilitator-server -p reference-merchant -p x402-bench -p setup-testnet"
  else
    (cd "$REPO_ROOT" && cargo build --release \
      -p x402-facilitator-server \
      -p reference-merchant \
      -p x402-bench \
      -p setup-testnet \
      2>&1 | tail -5)
    log "Local build complete."
  fi
else
  # Remote mode: setup each server in parallel
  SETUP_SCRIPT="
    set -euo pipefail
    if [[ -d ~/miden-x402 ]]; then
      cd ~/miden-x402 && git fetch origin && git checkout ${BRANCH} && git pull origin ${BRANCH}
    else
      git clone --depth 1 --branch ${BRANCH} ${REPO_URL} ~/miden-x402
    fi
    cd ~/miden-x402

    # Install Rust if missing
    if ! command -v cargo &>/dev/null; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.93.0
      . \"\\\$HOME/.cargo/env\"
    fi

    # System deps (Ubuntu)
    if command -v apt-get &>/dev/null; then
      sudo apt-get update -qq
      sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
        build-essential pkg-config libssl-dev cmake protobuf-compiler curl
    fi

    cargo build --release \
      -p x402-facilitator-server \
      -p reference-merchant \
      -p x402-bench \
      -p setup-testnet \
      2>&1 | tail -5
    echo 'Build complete.'
  "

  log "Setting up facilitator host (${FACILITATOR_HOST})..."
  run_on_prefixed "$FACILITATOR_HOST" "facilitator-setup" "$SETUP_SCRIPT" &
  PID_SETUP_FAC=$!

  log "Setting up merchant host (${MERCHANT_HOST})..."
  if [[ "$MERCHANT_HOST" != "$FACILITATOR_HOST" ]]; then
    run_on_prefixed "$MERCHANT_HOST" "merchant-setup" "$SETUP_SCRIPT" &
    PID_SETUP_MERCH=$!
  else
    PID_SETUP_MERCH=$PID_SETUP_FAC
  fi

  log "Setting up agent host (${AGENT_HOST})..."
  if [[ "$AGENT_HOST" != "$FACILITATOR_HOST" && "$AGENT_HOST" != "$MERCHANT_HOST" ]]; then
    run_on_prefixed "$AGENT_HOST" "agent-setup" "$SETUP_SCRIPT" &
    PID_SETUP_AGENT=$!
  else
    PID_SETUP_AGENT=$PID_SETUP_FAC
  fi

  log "Waiting for all builds to complete..."
  wait $PID_SETUP_FAC || { err "Facilitator setup failed"; exit 1; }
  if [[ "$MERCHANT_HOST" != "$FACILITATOR_HOST" ]]; then
    wait $PID_SETUP_MERCH || { err "Merchant setup failed"; exit 1; }
  fi
  if [[ "$AGENT_HOST" != "$FACILITATOR_HOST" && "$AGENT_HOST" != "$MERCHANT_HOST" ]]; then
    wait $PID_SETUP_AGENT || { err "Agent setup failed"; exit 1; }
  fi
  log "All builds complete."
fi

T_SETUP_END=$(now_ns)
SETUP_MS=$(( (T_SETUP_END - T_SETUP_START) / 1000000 ))
log "Setup completed in ${SETUP_MS}ms"
echo ""

###############################################################################
# Phase 2: Testnet account provisioning
###############################################################################

T_PROVISION_START=$(now_ns)
log "Phase 2: Provisioning testnet accounts"
echo ""

TESTNET_STATE_DIR="/tmp/miden-bench-state-${RUN_ID}"
SETUP_TOML="${TESTNET_STATE_DIR}/setup.toml"

if [[ "$LOCAL_MODE" == true ]]; then
  BINARY_DIR="${REPO_ROOT}/target/release"
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] ${BINARY_DIR}/setup-testnet --agents 1 --mint-amount $((NUM_PAYMENTS * AMOUNT_PER_PAYMENT * 2)) --out-dir ${TESTNET_STATE_DIR} --adn"
    # Create a fake setup.toml for dry-run
    mkdir -p "$TESTNET_STATE_DIR"
    cat > "$SETUP_TOML" <<DRYEOF
rpc_endpoint = "${MIDEN_RPC}"
faucet_id_hex = "0x0000000000000001"
faucet_id_bech32 = "test1faucet"
merchant_id_hex = "0x0000000000000002"
merchant_id_bech32 = "test1merchant"
agent_count = 1
mint_amount = $((NUM_PAYMENTS * AMOUNT_PER_PAYMENT * 2))
adn_note_id = "0xdeadbeef"
adn_serial_num_hex = ["0x0", "0x0", "0x0", "0x0"]
adn_balance = $((NUM_PAYMENTS * AMOUNT_PER_PAYMENT * 2))
adn_expiry_block = 999999

[[agents]]
index = 0
account_id_hex = "0x0000000000000003"
account_id_bech32 = "test1agent"
snapshot_path = "agent-0-snapshot.b64"
hot_key_path = "agent-0-hotkey.bin"
commitment_hex = "0x0000000000000000000000000000000000000000000000000000000000000000"
DRYEOF
  else
    log "Running setup-testnet (creating faucet, agent, merchant, ADN note)..."
    log "  This interacts with Miden testnet and may take 2-5 minutes."
    mkdir -p "$TESTNET_STATE_DIR"
    "${BINARY_DIR}/setup-testnet" \
      --agents 1 \
      --mint-amount $((NUM_PAYMENTS * AMOUNT_PER_PAYMENT * 2)) \
      --out-dir "$TESTNET_STATE_DIR" \
      --adn \
      2>&1 | sed 's/^/  [setup-testnet] /'
  fi
else
  # Run setup-testnet on the facilitator host (it needs the accounts)
  log "Running setup-testnet on facilitator host..."
  run_on_prefixed "$FACILITATOR_HOST" "setup-testnet" "
    cd ~/miden-x402
    mkdir -p testnet-state
    ./target/release/setup-testnet \
      --agents 1 \
      --mint-amount $((NUM_PAYMENTS * AMOUNT_PER_PAYMENT * 2)) \
      --out-dir ./testnet-state \
      --adn \
      2>&1
  "
  # Pull setup.toml locally
  mkdir -p "$TESTNET_STATE_DIR"
  SCP_KEY_OPT=""
  if [[ -n "$SSH_KEY" ]]; then
    SCP_KEY_OPT="-i $SSH_KEY"
  fi
  # shellcheck disable=SC2086
  scp $SCP_KEY_OPT $SSH_OPTS -r \
    "${SSH_USER}@${FACILITATOR_HOST}:~/miden-x402/testnet-state/*" \
    "$TESTNET_STATE_DIR/"
fi

# Parse setup.toml
if [[ "$DRY_RUN" != true ]]; then
  if [[ ! -f "$SETUP_TOML" ]]; then
    err "setup.toml not found at ${SETUP_TOML}"
    exit 1
  fi
fi

# Extract IDs using simple grep/sed (no toml parser needed)
extract_toml_val() {
  local key="$1"
  local file="$2"
  sed -n "s/^${key} *= *\"\([^\"]*\)\".*/\1/p" "$file" | head -1
}

FAUCET_ID=$(extract_toml_val "faucet_id_hex" "$SETUP_TOML")
MERCHANT_ID=$(extract_toml_val "merchant_id_hex" "$SETUP_TOML")
ADN_NOTE_ID=$(extract_toml_val "adn_note_id" "$SETUP_TOML")

log "Faucet ID:    ${FAUCET_ID:-<not set>}"
log "Merchant ID:  ${MERCHANT_ID:-<not set>}"
log "ADN Note ID:  ${ADN_NOTE_ID:-<not set>}"

# Save config for reference
cp "$SETUP_TOML" "${RESULTS_DIR}/${RUN_ID}/setup.toml" 2>/dev/null || true

T_PROVISION_END=$(now_ns)
PROVISION_MS=$(( (T_PROVISION_END - T_PROVISION_START) / 1000000 ))
log "Provisioning completed in ${PROVISION_MS}ms"
echo ""

###############################################################################
# Phase 3: Start servers
###############################################################################

log "Phase 3: Starting facilitator and merchant servers"
echo ""

# Determine the facilitator URL that the merchant will use
if [[ "$MERCHANT_HOST" == "$FACILITATOR_HOST" || "$LOCAL_MODE" == true ]]; then
  FACILITATOR_INTERNAL_URL="http://localhost:${FACILITATOR_PORT}"
else
  FACILITATOR_INTERNAL_URL="http://${FACILITATOR_HOST}:${FACILITATOR_PORT}"
fi

# Facilitator URL for the agent / external callers
if [[ "$LOCAL_MODE" == true ]]; then
  FACILITATOR_URL="http://localhost:${FACILITATOR_PORT}"
  MERCHANT_URL="http://localhost:${MERCHANT_PORT}"
else
  FACILITATOR_URL="http://${FACILITATOR_HOST}:${FACILITATOR_PORT}"
  MERCHANT_URL="http://${MERCHANT_HOST}:${MERCHANT_PORT}"
fi

# --- Start facilitator ---
log "Starting facilitator on ${FACILITATOR_HOST}:${FACILITATOR_PORT}..."

if [[ "$LOCAL_MODE" == true ]]; then
  FACILITATOR_LOG="/tmp/miden-bench-facilitator-${RUN_ID}.log"
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] FACILITATOR_HTTP_PORT=${FACILITATOR_PORT} ${BINARY_DIR}/x402-facilitator-server"
  else
    FACILITATOR_DATA_DIR="/tmp/miden-bench-facilitator-data-${RUN_ID}" \
    FACILITATOR_HTTP_PORT="${FACILITATOR_PORT}" \
    MIDEN_RPC_ENDPOINT="${MIDEN_RPC}" \
    RUST_LOG=info \
      nohup "${BINARY_DIR}/x402-facilitator-server" > "$FACILITATOR_LOG" 2>&1 &
    FACILITATOR_PID=$!
    log "Facilitator started (PID ${FACILITATOR_PID})"
  fi
else
  FACILITATOR_PID=$(start_bg "$FACILITATOR_HOST" "~/miden-x402/facilitator.log" "
    cd ~/miden-x402 &&
    FACILITATOR_DATA_DIR=./facilitator-data \
    FACILITATOR_HTTP_PORT=${FACILITATOR_PORT} \
    MIDEN_RPC_ENDPOINT=${MIDEN_RPC} \
    RUST_LOG=info \
    ./target/release/x402-facilitator-server
  ")
  log "Facilitator started (remote PID ${FACILITATOR_PID})"
fi

# Wait for facilitator to be healthy
if [[ "$DRY_RUN" != true ]]; then
  wait_for_http "${FACILITATOR_URL}/healthz" "facilitator" 60 || {
    # Try alternative health endpoint
    wait_for_http "${FACILITATOR_URL}/health" "facilitator" 10 || {
      err "Facilitator failed to start. Check logs."
      if [[ "$LOCAL_MODE" == true ]]; then
        tail -20 "$FACILITATOR_LOG" 2>/dev/null || true
      fi
      exit 1
    }
  }
fi

# --- Start merchant ---
log "Starting merchant on ${MERCHANT_HOST}:${MERCHANT_PORT}..."

if [[ "$LOCAL_MODE" == true ]]; then
  MERCHANT_LOG="/tmp/miden-bench-merchant-${RUN_ID}.log"
  if [[ "$DRY_RUN" == true ]]; then
    echo "  [dry-run] MERCHANT_HTTP_PORT=${MERCHANT_PORT} ${BINARY_DIR}/reference-merchant"
  else
    MERCHANT_ACCOUNT_ID="${MERCHANT_ID}" \
    MERCHANT_ASSET_FAUCET_ID="${FAUCET_ID}" \
    MERCHANT_PRICE_AMOUNT="${AMOUNT_PER_PAYMENT}" \
    MERCHANT_HTTP_PORT="${MERCHANT_PORT}" \
    FACILITATOR_URL="${FACILITATOR_INTERNAL_URL}" \
    RUST_LOG=info \
      nohup "${BINARY_DIR}/reference-merchant" > "$MERCHANT_LOG" 2>&1 &
    MERCHANT_PID=$!
    log "Merchant started (PID ${MERCHANT_PID})"
  fi
else
  MERCHANT_PID=$(start_bg "$MERCHANT_HOST" "~/miden-x402/merchant.log" "
    cd ~/miden-x402 &&
    MERCHANT_ACCOUNT_ID=${MERCHANT_ID} \
    MERCHANT_ASSET_FAUCET_ID=${FAUCET_ID} \
    MERCHANT_PRICE_AMOUNT=${AMOUNT_PER_PAYMENT} \
    MERCHANT_HTTP_PORT=${MERCHANT_PORT} \
    FACILITATOR_URL=${FACILITATOR_INTERNAL_URL} \
    RUST_LOG=info \
    ./target/release/reference-merchant
  ")
  log "Merchant started (remote PID ${MERCHANT_PID})"
fi

# Wait for merchant
if [[ "$DRY_RUN" != true ]]; then
  wait_for_http "${MERCHANT_URL}/health" "merchant" 60 || {
    wait_for_http "${MERCHANT_URL}/resource" "merchant" 10 || {
      err "Merchant failed to start. Check logs."
      if [[ "$LOCAL_MODE" == true ]]; then
        tail -20 "$MERCHANT_LOG" 2>/dev/null || true
      fi
      exit 1
    }
  }
fi

log "Both servers are running."
echo ""

###############################################################################
# Phase 4: Run the benchmark
###############################################################################

T_BENCH_START=$(now_ns)
log "Phase 4: Running benchmark (${NUM_PAYMENTS} payments, scheme=adn)"
echo ""

BENCH_OUT_DIR="${RESULTS_DIR}/${RUN_ID}/bench-out"

if [[ "$LOCAL_MODE" == true ]]; then
  BENCH_BIN="${BINARY_DIR}/x402-bench"
  BENCH_CMD="${BENCH_BIN} \
    --setup-dir ${TESTNET_STATE_DIR} \
    --miden-rpc ${MIDEN_RPC} \
    --agents 1 \
    --payments ${NUM_PAYMENTS} \
    --facilitator-url ${FACILITATOR_URL} \
    --merchant-url ${MERCHANT_URL} \
    --out-dir ${BENCH_OUT_DIR} \
    --scheme adn"
else
  # Copy testnet-state to agent host if it differs
  if [[ "$AGENT_HOST" != "$FACILITATOR_HOST" ]]; then
    log "Copying testnet-state to agent host..."
    SCP_KEY_OPT=""
    if [[ -n "$SSH_KEY" ]]; then
      SCP_KEY_OPT="-i $SSH_KEY"
    fi
    # shellcheck disable=SC2086
    scp $SCP_KEY_OPT $SSH_OPTS -r \
      "$TESTNET_STATE_DIR" \
      "${SSH_USER}@${AGENT_HOST}:~/miden-x402/testnet-state"
  fi

  BENCH_CMD="cd ~/miden-x402 && ./target/release/x402-bench \
    --setup-dir ./testnet-state \
    --miden-rpc ${MIDEN_RPC} \
    --agents 1 \
    --payments ${NUM_PAYMENTS} \
    --facilitator-url ${FACILITATOR_URL} \
    --merchant-url ${MERCHANT_URL} \
    --out-dir ./bench-out \
    --scheme adn"
fi

if [[ "$DRY_RUN" == true ]]; then
  echo "  [dry-run] ${BENCH_CMD}"
else
  log "--- ADN Benchmark: ${NUM_PAYMENTS} payments ---"
  echo ""

  if [[ "$LOCAL_MODE" == true ]]; then
    eval "$BENCH_CMD" 2>&1 | sed 's/^/  [bench] /'
  else
    run_on_prefixed "$AGENT_HOST" "bench" "$BENCH_CMD"
    # Copy results back
    SCP_KEY_OPT=""
    if [[ -n "$SSH_KEY" ]]; then
      SCP_KEY_OPT="-i $SSH_KEY"
    fi
    # shellcheck disable=SC2086
    scp $SCP_KEY_OPT $SSH_OPTS -r \
      "${SSH_USER}@${AGENT_HOST}:~/miden-x402/bench-out" \
      "$BENCH_OUT_DIR" 2>/dev/null || true
  fi
fi

T_BENCH_END=$(now_ns)
BENCH_MS=$(( (T_BENCH_END - T_BENCH_START) / 1000000 ))
log "Benchmark completed in ${BENCH_MS}ms"
echo ""

###############################################################################
# Phase 5: Collect results and print summary
###############################################################################

log "Phase 5: Results summary"
echo ""

# Find the summary CSV
SUMMARY_CSV=$(find "${BENCH_OUT_DIR}" -name "summary.csv" -type f 2>/dev/null | sort | tail -1)
PAYMENTS_CSV=$(find "${BENCH_OUT_DIR}" -name "payments.csv" -type f 2>/dev/null | sort | tail -1)

if [[ -n "$SUMMARY_CSV" && "$DRY_RUN" != true ]]; then
  echo "============================================"
  echo "  BENCHMARK RESULTS"
  echo "============================================"
  echo ""

  # Parse summary CSV for display
  echo "  Percentile Latencies:"
  echo "  ─────────────────────────────────────────"
  # Skip header, format each row
  tail -n +2 "$SUMMARY_CSV" | while IFS=',' read -r metric count p50 p95 p99 mean min_val max_val; do
    if [[ -n "$p50" && "$p50" != "" ]]; then
      printf "  %-20s  p50=%6s  p95=%6s  mean=%6s  (n=%s)\n" \
        "$metric" "${p50}us" "${p95}us" "${mean}us" "$count"
    else
      printf "  %-20s  count=%s\n" "$metric" "$count"
    fi
  done
  echo ""

  # Calculate throughput from payments CSV
  if [[ -n "$PAYMENTS_CSV" ]]; then
    OK_COUNT=$(tail -n +2 "$PAYMENTS_CSV" | grep -c ',true,' || echo 0)
    ERR_COUNT=$(tail -n +2 "$PAYMENTS_CSV" | grep -c ',false,' || echo 0)
    TOTAL_COUNT=$((OK_COUNT + ERR_COUNT))

    if [[ $OK_COUNT -gt 0 ]]; then
      # Get first and last timestamps for throughput
      FIRST_T=$(tail -n +2 "$PAYMENTS_CSV" | head -1 | cut -d',' -f4)
      LAST_T=$(tail -n +2 "$PAYMENTS_CSV" | tail -1 | cut -d',' -f12)
      if [[ -n "$FIRST_T" && -n "$LAST_T" && "$FIRST_T" != "0" ]]; then
        DURATION_US=$((LAST_T - FIRST_T))
        if [[ $DURATION_US -gt 0 ]]; then
          # Throughput = count / duration_seconds
          THROUGHPUT=$(python3 -c "print(f'{${OK_COUNT} / (${DURATION_US} / 1e6):.1f}')" 2>/dev/null || echo "N/A")
          echo "  Throughput:  ${THROUGHPUT} vouchers/second"
        fi
      fi
    fi

    echo "  Successful: ${OK_COUNT} / ${TOTAL_COUNT}"
    if [[ $ERR_COUNT -gt 0 ]]; then
      echo "  Errors:     ${ERR_COUNT}"
    fi
    echo ""
  fi

  # Per-phase timing
  echo "  Phase Timings:"
  echo "  ─────────────────────────────────────────"
  printf "  %-25s %8s ms\n" "Setup (build):" "$SETUP_MS"
  printf "  %-25s %8s ms\n" "Provisioning (testnet):" "$PROVISION_MS"
  printf "  %-25s %8s ms\n" "Benchmark run:" "$BENCH_MS"
  TOTAL_MS=$(( SETUP_MS + PROVISION_MS + BENCH_MS ))
  printf "  %-25s %8s ms\n" "Total end-to-end:" "$TOTAL_MS"
  echo ""

elif [[ "$DRY_RUN" == true ]]; then
  echo "  [dry-run] Would display results from ${BENCH_OUT_DIR}"
else
  warn "No summary.csv found in ${BENCH_OUT_DIR}"
  log "Listing output directory:"
  find "$BENCH_OUT_DIR" -type f 2>/dev/null | sed 's/^/  /' || echo "  (empty)"
fi

echo "============================================"
echo "  Results saved to: ${RESULTS_DIR}/${RUN_ID}/"
echo "============================================"
echo ""

# Copy summary to results dir root for easy access
if [[ -n "$SUMMARY_CSV" ]]; then
  cp "$SUMMARY_CSV" "${RESULTS_DIR}/${RUN_ID}/summary.csv" 2>/dev/null || true
fi
if [[ -n "$PAYMENTS_CSV" ]]; then
  cp "$PAYMENTS_CSV" "${RESULTS_DIR}/${RUN_ID}/payments.csv" 2>/dev/null || true
fi

log "Done. Servers will be stopped on exit."
