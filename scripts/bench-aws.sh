#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# bench-aws.sh — Run ADN batch-settlement benchmark across 3 AWS EC2 instances
#
# Usage:
#   ./scripts/bench-aws.sh \
#     --agent-ip 3.14.15.92 \
#     --merchant-ip 3.14.15.93 \
#     --facilitator-ip 3.14.15.94 \
#     --payments 100
#
# Prerequisites:
#   - 3 EC2 instances (Ubuntu, t3.xlarge or better for Rust compilation)
#   - SSH key access (default: ~/.ssh/id_rsa, override with --ssh-key)
#   - Security groups: allow ports 7001, 7002 between instances
#   - Rust toolchain installed on all instances (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)
###############################################################################

# ─── Defaults ────────────────────────────────────────────────────────────────
AGENT_IP=""
MERCHANT_IP=""
FACILITATOR_IP=""
PAYMENTS=100
AMOUNT_PER_PAYMENT=1000
SETTLE_AFTER=50
ADN_BALANCE=500000
SSH_USER="${BENCH_SSH_USER:-ubuntu}"
SSH_KEY="${BENCH_SSH_KEY:-$HOME/.ssh/id_rsa}"
BRANCH="${BENCH_BRANCH:-main}"
REPO_URL="https://github.com/Dominik1999/miden-x402.git"
REPO_DIR="/home/${SSH_USER}/miden-x402"

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Required:
  --agent-ip IP          Agent EC2 instance IP
  --merchant-ip IP       Merchant EC2 instance IP
  --facilitator-ip IP    Facilitator EC2 instance IP

Optional:
  --payments N           Number of voucher payments (default: 100)
  --amount N             Amount per payment (default: 1000)
  --settle-after N       Settle every N requests (default: 50)
  --adn-balance N        Total ADN note balance (default: 500000)
  --ssh-user USER        SSH user (default: ubuntu)
  --ssh-key PATH         SSH key path (default: ~/.ssh/id_rsa)
  --branch BRANCH        Git branch to build (default: main)
  -h, --help             Show this help

Environment:
  BENCH_SSH_USER         Override SSH user
  BENCH_SSH_KEY          Override SSH key path
  BENCH_BRANCH           Override git branch
EOF
  exit 0
}

# ─── Parse args ──────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --agent-ip)       AGENT_IP="$2"; shift 2 ;;
    --merchant-ip)    MERCHANT_IP="$2"; shift 2 ;;
    --facilitator-ip) FACILITATOR_IP="$2"; shift 2 ;;
    --payments)       PAYMENTS="$2"; shift 2 ;;
    --amount)         AMOUNT_PER_PAYMENT="$2"; shift 2 ;;
    --settle-after)   SETTLE_AFTER="$2"; shift 2 ;;
    --adn-balance)    ADN_BALANCE="$2"; shift 2 ;;
    --ssh-user)       SSH_USER="$2"; shift 2 ;;
    --ssh-key)        SSH_KEY="$2"; shift 2 ;;
    --branch)         BRANCH="$2"; shift 2 ;;
    -h|--help)        usage ;;
    *) echo "Unknown option: $1"; usage ;;
  esac
done

if [[ -z "$AGENT_IP" || -z "$MERCHANT_IP" || -z "$FACILITATOR_IP" ]]; then
  echo "ERROR: --agent-ip, --merchant-ip, and --facilitator-ip are required"
  echo ""
  usage
fi

SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -i $SSH_KEY"

log() { echo "[$(date +%H:%M:%S)] $*"; }

ssh_run() {
  local host="$1"; shift
  ssh $SSH_OPTS "${SSH_USER}@${host}" "$@"
}

scp_to() {
  local src="$1" host="$2" dst="$3"
  scp $SSH_OPTS "$src" "${SSH_USER}@${host}:${dst}"
}

scp_dir_to() {
  local src="$1" host="$2" dst="$3"
  scp -r $SSH_OPTS "$src" "${SSH_USER}@${host}:${dst}"
}

# ─── Cleanup on exit ────────────────────────────────────────────────────────
cleanup() {
  log "Cleaning up remote servers..."
  for ip in "$FACILITATOR_IP" "$MERCHANT_IP"; do
    ssh_run "$ip" "pkill -f adn-facilitator 2>/dev/null; pkill -f adn-merchant 2>/dev/null" || true
  done
  log "Done."
}
trap cleanup EXIT

# ═══════════════════════════════════════════════════════════════════════════
log "═══ ADN BATCH-SETTLEMENT AWS BENCHMARK ═══"
log "  Agent:       $AGENT_IP"
log "  Merchant:    $MERCHANT_IP"
log "  Facilitator: $FACILITATOR_IP"
log "  Payments:    $PAYMENTS"
log "  Amount/pay:  $AMOUNT_PER_PAYMENT"
log "  Settle after: $SETTLE_AFTER"
log "  ADN balance: $ADN_BALANCE"
log ""

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 1: Build on all servers (parallel)
# ═══════════════════════════════════════════════════════════════════════════
log "═══ PHASE 1: BUILD ═══"

build_on_host() {
  local ip="$1" name="$2"
  log "[$name] Building on $ip..."
  ssh_run "$ip" "
    if [ -d ${REPO_DIR} ]; then
      cd ${REPO_DIR} && git fetch origin && git checkout ${BRANCH} && git pull origin ${BRANCH}
    else
      git clone --branch ${BRANCH} ${REPO_URL} ${REPO_DIR}
      cd ${REPO_DIR}
    fi
    cd ${REPO_DIR}
    source \$HOME/.cargo/env 2>/dev/null || true
    cargo build --release -p adn-services 2>&1 | tail -3
  "
  log "[$name] Build complete on $ip"
}

build_on_host "$AGENT_IP"       "agent"       &
build_on_host "$MERCHANT_IP"    "merchant"    &
build_on_host "$FACILITATOR_IP" "facilitator" &
wait
log "All builds complete."

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 2: Setup (on agent server)
# ═══════════════════════════════════════════════════════════════════════════
log ""
log "═══ PHASE 2: SETUP (agent creates accounts + ADN note on testnet) ═══"

ssh_run "$AGENT_IP" "
  source \$HOME/.cargo/env 2>/dev/null || true
  rm -rf /tmp/adn-bench /tmp/bench-config.json
  cd ${REPO_DIR}
  ./target/release/adn-agent setup \
    --data-dir /tmp/adn-bench \
    --out-config /tmp/bench-config.json \
    --adn-balance $ADN_BALANCE \
    2>&1
"
log "Setup complete. Copying config to other servers..."

# Copy config + keystore to facilitator and merchant
scp_to "${SSH_USER}@${AGENT_IP}:/tmp/bench-config.json" "$FACILITATOR_IP" "/tmp/bench-config.json"
scp_to "${SSH_USER}@${AGENT_IP}:/tmp/bench-config.json" "$MERCHANT_IP" "/tmp/bench-config.json"

# For SCP from agent to facilitator, we need to go through local
ssh_run "$AGENT_IP" "tar czf /tmp/adn-keystore.tar.gz -C /tmp/adn-bench keystore"
scp $SSH_OPTS "${SSH_USER}@${AGENT_IP}:/tmp/adn-keystore.tar.gz" /tmp/adn-keystore.tar.gz
scp_to "/tmp/adn-keystore.tar.gz" "$FACILITATOR_IP" "/tmp/adn-keystore.tar.gz"
ssh_run "$FACILITATOR_IP" "mkdir -p /tmp/adn-bench && tar xzf /tmp/adn-keystore.tar.gz -C /tmp/adn-bench"
log "Config + keystore distributed."

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 3: Start facilitator
# ═══════════════════════════════════════════════════════════════════════════
log ""
log "═══ PHASE 3: START FACILITATOR ═══"

ssh_run "$FACILITATOR_IP" "
  source \$HOME/.cargo/env 2>/dev/null || true
  rm -rf /tmp/adn-facilitator
  python3 -c \"import json; print(json.load(open('/tmp/bench-config.json'))['facilitator_account_b64'])\" > /tmp/fac.b64
  cd ${REPO_DIR}
  nohup ./target/release/adn-facilitator \
    --port 7002 \
    --data-dir /tmp/adn-facilitator \
    --account-file /tmp/fac.b64 \
    --import-keystore /tmp/adn-bench/keystore \
    > /tmp/facilitator.log 2>&1 &
  echo \$!
"

# Wait for facilitator to be ready
log "Waiting for facilitator to start..."
for i in $(seq 1 30); do
  if ssh_run "$FACILITATOR_IP" "curl -s -o /dev/null -w '%{http_code}' http://localhost:7002/verify -X POST -H 'Content-Type: application/json' -d '{\"note_file_hex\":\"00\",\"merchant_id_hex\":\"0x00\"}'" 2>/dev/null | grep -q 200; then
    log "Facilitator ready."
    break
  fi
  sleep 2
done

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 4: Start merchant
# ═══════════════════════════════════════════════════════════════════════════
log ""
log "═══ PHASE 4: START MERCHANT ═══"

ssh_run "$MERCHANT_IP" "
  source \$HOME/.cargo/env 2>/dev/null || true
  MERCHANT_ID=\$(python3 -c \"import json; print(json.load(open('/tmp/bench-config.json'))['merchant_id_hex'])\")
  cd ${REPO_DIR}
  nohup ./target/release/adn-merchant \
    --port 7001 \
    --facilitator-url http://${FACILITATOR_IP}:7002 \
    --merchant-id \"\$MERCHANT_ID\" \
    --settle-after $SETTLE_AFTER \
    > /tmp/merchant.log 2>&1 &
  echo \$!
"

# Wait for merchant to be ready
log "Waiting for merchant to start..."
for i in $(seq 1 15); do
  if ssh_run "$MERCHANT_IP" "curl -s -o /dev/null -w '%{http_code}' http://localhost:7001/resource" 2>/dev/null | grep -q 402; then
    log "Merchant ready."
    break
  fi
  sleep 1
done

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 5: Run benchmark
# ═══════════════════════════════════════════════════════════════════════════
log ""
log "═══ PHASE 5: BENCHMARK ═══"

ssh_run "$AGENT_IP" "
  source \$HOME/.cargo/env 2>/dev/null || true
  cd ${REPO_DIR}
  ./target/release/adn-agent benchmark \
    --config /tmp/bench-config.json \
    --merchant-url http://${MERCHANT_IP}:7001 \
    --payments $PAYMENTS \
    --amount-per-payment $AMOUNT_PER_PAYMENT \
    2>&1
"

# ═══════════════════════════════════════════════════════════════════════════
# PHASE 6: Collect logs
# ═══════════════════════════════════════════════════════════════════════════
log ""
log "═══ PHASE 6: LOGS ═══"

RESULTS_DIR="bench-results/aws-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$RESULTS_DIR"

scp $SSH_OPTS "${SSH_USER}@${FACILITATOR_IP}:/tmp/facilitator.log" "$RESULTS_DIR/" 2>/dev/null || true
scp $SSH_OPTS "${SSH_USER}@${MERCHANT_IP}:/tmp/merchant.log" "$RESULTS_DIR/" 2>/dev/null || true

log "Logs saved to $RESULTS_DIR/"
log ""
log "═══ BENCHMARK COMPLETE ═══"
