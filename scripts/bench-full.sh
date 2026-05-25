#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# bench-full.sh — One-command AWS benchmark for ADN batch-settlement
#
# Launches 3 EC2 instances, builds, runs 50 payments, prints results, terminates.
#
# Usage:
#   ./scripts/bench-full.sh
#   ./scripts/bench-full.sh --payments 100 --region us-east-1
#   ./scripts/bench-full.sh --keep   # don't terminate instances after
###############################################################################

PAYMENTS=${BENCH_PAYMENTS:-50}
AMOUNT=1000
SETTLE_AFTER=25
ADN_BALANCE=500000
REGION="${AWS_DEFAULT_REGION:-us-east-1}"
FACILITATOR_REGION=""  # If set, facilitator runs in a different region
INSTANCE_TYPE="${BENCH_INSTANCE_TYPE:-t3.xlarge}"
AMI=""  # auto-detect Ubuntu 22.04
KEY_NAME="${BENCH_KEY_NAME:-}"
KEY_FILE="${BENCH_KEY_FILE:-}"
SECURITY_GROUP=""
KEEP=false
BRANCH="main"
REPO_URL="https://github.com/Dominik1999/miden-x402.git"
SSH_USER="ubuntu"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --payments)       PAYMENTS="$2"; shift 2 ;;
    --region)         REGION="$2"; shift 2 ;;
    --instance-type)  INSTANCE_TYPE="$2"; shift 2 ;;
    --key-name)       KEY_NAME="$2"; shift 2 ;;
    --key-file)       KEY_FILE="$2"; shift 2 ;;
    --keep)           KEEP=true; shift ;;
    --branch)         BRANCH="$2"; shift 2 ;;
    --facilitator-region) FACILITATOR_REGION="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 [--payments N] [--region REGION] [--facilitator-region REGION] [--instance-type TYPE] [--key-name NAME] [--key-file PATH] [--keep] [--branch BRANCH]"
      echo ""
      echo "Env vars: AWS_DEFAULT_REGION, BENCH_KEY_NAME, BENCH_KEY_FILE, BENCH_INSTANCE_TYPE, BENCH_PAYMENTS"
      exit 0 ;;
    *) echo "Unknown: $1"; exit 1 ;;
  esac
done

# ─── Preflight ───────────────────────────────────────────────────────────────
for cmd in aws jq ssh scp; do
  command -v "$cmd" >/dev/null || { echo "ERROR: $cmd not found"; exit 1; }
done

if [[ -z "$KEY_NAME" ]]; then
  # Try to find an existing key pair
  KEY_NAME=$(aws ec2 describe-key-pairs --region "$REGION" --query 'KeyPairs[0].KeyName' --output text 2>/dev/null || true)
  if [[ -z "$KEY_NAME" || "$KEY_NAME" == "None" ]]; then
    echo "ERROR: No SSH key pair found. Set --key-name or BENCH_KEY_NAME"
    exit 1
  fi
  echo "Using key pair: $KEY_NAME"
fi

if [[ -z "$KEY_FILE" ]]; then
  # Try common locations
  for f in "$HOME/.ssh/${KEY_NAME}.pem" "$HOME/.ssh/${KEY_NAME}" "$HOME/.ssh/id_rsa" "$HOME/.ssh/id_ed25519"; do
    if [[ -f "$f" ]]; then KEY_FILE="$f"; break; fi
  done
  if [[ -z "$KEY_FILE" ]]; then
    echo "ERROR: Cannot find SSH key file. Set --key-file or BENCH_KEY_FILE"
    exit 1
  fi
  echo "Using key file: $KEY_FILE"
fi

SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=30 -o ServerAliveInterval=15 -i $KEY_FILE"

log() { echo "[$(date +%H:%M:%S)] $*"; }

# ─── Find Ubuntu AMI ────────────────────────────────────────────────────────
if [[ -z "$AMI" ]]; then
  AMI=$(aws ec2 describe-images \
    --region "$REGION" \
    --owners 099720109477 \
    --filters "Name=name,Values=ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*" \
              "Name=state,Values=available" \
    --query 'Images | sort_by(@, &CreationDate) | [-1].ImageId' \
    --output text)
  log "Ubuntu 22.04 AMI: $AMI"
fi

# ─── Create security group ──────────────────────────────────────────────────
SG_NAME="adn-bench-$(date +%s)"
SECURITY_GROUP=$(aws ec2 create-security-group \
  --region "$REGION" \
  --group-name "$SG_NAME" \
  --description "ADN benchmark - auto-created" \
  --query 'GroupId' --output text)
log "Security group: $SECURITY_GROUP"

aws ec2 authorize-security-group-ingress --region "$REGION" --group-id "$SECURITY_GROUP" \
  --protocol tcp --port 22 --cidr 0.0.0.0/0 >/dev/null
aws ec2 authorize-security-group-ingress --region "$REGION" --group-id "$SECURITY_GROUP" \
  --protocol tcp --port 7001-7002 --source-group "$SECURITY_GROUP" >/dev/null

# ─── User data: install Rust ────────────────────────────────────────────────
USER_DATA=$(cat <<'CLOUDINIT'
#!/bin/bash
apt-get update -y && apt-get install -y build-essential pkg-config libssl-dev git python3 curl
su - ubuntu -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
su - ubuntu -c 'echo "source ~/.cargo/env" >> ~/.bashrc'
touch /tmp/rust-ready
CLOUDINIT
)

# ─── Launch 3 instances ─────────────────────────────────────────────────────
log ""
log "═══ LAUNCHING 3 EC2 INSTANCES ($INSTANCE_TYPE in $REGION) ═══"

INSTANCE_IDS=$(aws ec2 run-instances \
  --region "$REGION" \
  --image-id "$AMI" \
  --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --security-group-ids "$SECURITY_GROUP" \
  --count 3 \
  --user-data "$USER_DATA" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=adn-bench},{Key=Purpose,Value=adn-benchmark}]" \
  --query 'Instances[*].InstanceId' --output text)

IDS=($INSTANCE_IDS)
AGENT_ID="${IDS[0]}"
MERCHANT_ID="${IDS[1]}"
FACILITATOR_ID="${IDS[2]}"

log "  Agent:       $AGENT_ID"
log "  Merchant:    $MERCHANT_ID"
log "  Facilitator: $FACILITATOR_ID"

# ─── Cleanup trap ────────────────────────────────────────────────────────────
cleanup() {
  if [[ "$KEEP" == "true" ]]; then
    log "Keeping instances alive (--keep). Terminate manually:"
    log "  aws ec2 terminate-instances --region $REGION --instance-ids $INSTANCE_IDS"
  else
    log "Terminating instances..."
    aws ec2 terminate-instances --region "$REGION" --instance-ids $INSTANCE_IDS >/dev/null 2>&1 || true
    # Wait a bit then delete security group
    sleep 5
    aws ec2 delete-security-group --region "$REGION" --group-id "$SECURITY_GROUP" 2>/dev/null || true
    log "Instances terminated, security group deleted."
  fi
}
trap cleanup EXIT

# ─── Wait for instances to be running ────────────────────────────────────────
log "Waiting for instances to be running..."
aws ec2 wait instance-running --region "$REGION" --instance-ids $INSTANCE_IDS
log "All instances running."

# Get public IPs
AGENT_IP=$(aws ec2 describe-instances --region "$REGION" --instance-ids "$AGENT_ID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
MERCHANT_IP=$(aws ec2 describe-instances --region "$REGION" --instance-ids "$MERCHANT_ID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
FACILITATOR_IP=$(aws ec2 describe-instances --region "$REGION" --instance-ids "$FACILITATOR_ID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)

log "  Agent:       $AGENT_IP"
log "  Merchant:    $MERCHANT_IP"
log "  Facilitator: $FACILITATOR_IP"

# ─── Wait for Rust to be installed ───────────────────────────────────────────
log "Waiting for Rust installation (cloud-init)..."
for ip in "$AGENT_IP" "$MERCHANT_IP" "$FACILITATOR_IP"; do
  for attempt in $(seq 1 60); do
    if ssh $SSH_OPTS "${SSH_USER}@${ip}" "test -f /tmp/rust-ready" 2>/dev/null; then
      break
    fi
    if [[ $attempt -eq 60 ]]; then
      echo "ERROR: Rust install timed out on $ip"
      exit 1
    fi
    sleep 10
  done
done
log "Rust ready on all instances."

# ─── Helper ──────────────────────────────────────────────────────────────────
ssh_run() {
  local ip="$1"; shift
  ssh $SSH_OPTS "${SSH_USER}@${ip}" "$@"
}

# ─── Build on all servers (parallel) ────────────────────────────────────────
log ""
log "═══ BUILDING ON ALL SERVERS ═══"

build_on() {
  local ip="$1" name="$2"
  log "[$name] Building on $ip..."
  ssh_run "$ip" "
    source ~/.cargo/env
    git clone --branch ${BRANCH} ${REPO_URL} ~/miden-x402 2>/dev/null || (cd ~/miden-x402 && git pull)
    cd ~/miden-x402
    cargo build --release -p adn-services 2>&1 | tail -3
  "
  log "[$name] Build done."
}

build_on "$AGENT_IP"       "agent"       &
build_on "$MERCHANT_IP"    "merchant"    &
build_on "$FACILITATOR_IP" "facilitator" &
wait
log "All builds complete."

# ─── Setup (agent creates accounts + ADN on testnet) ────────────────────────
log ""
log "═══ SETUP (creating accounts + ADN note on Miden testnet) ═══"

ssh_run "$AGENT_IP" "
  source ~/.cargo/env
  cd ~/miden-x402
  ./target/release/adn-agent setup \
    --data-dir /tmp/adn-bench \
    --out-config /tmp/bench-config.json \
    --adn-balance $ADN_BALANCE 2>&1
"
log "Setup complete."

# ─── Distribute config ──────────────────────────────────────────────────────
log "Distributing config..."

# Agent → local → facilitator/merchant
scp $SSH_OPTS "${SSH_USER}@${AGENT_IP}:/tmp/bench-config.json" /tmp/bench-config-aws.json
scp -r $SSH_OPTS "${SSH_USER}@${AGENT_IP}:/tmp/adn-bench/keystore" /tmp/adn-keystore-aws

scp $SSH_OPTS /tmp/bench-config-aws.json "${SSH_USER}@${FACILITATOR_IP}:/tmp/bench-config.json"
scp $SSH_OPTS /tmp/bench-config-aws.json "${SSH_USER}@${MERCHANT_IP}:/tmp/bench-config.json"
ssh_run "$FACILITATOR_IP" "mkdir -p /tmp/adn-bench"
scp -r $SSH_OPTS /tmp/adn-keystore-aws "${SSH_USER}@${FACILITATOR_IP}:/tmp/adn-bench/keystore"

# ─── Start facilitator ──────────────────────────────────────────────────────
log ""
log "═══ STARTING FACILITATOR ═══"

ssh_run "$FACILITATOR_IP" "
  source ~/.cargo/env
  rm -rf /tmp/adn-facilitator
  python3 -c \"import json; print(json.load(open('/tmp/bench-config.json'))['facilitator_account_b64'])\" > /tmp/fac.b64
  cd ~/miden-x402
  nohup ./target/release/adn-facilitator \
    --port 7002 \
    --data-dir /tmp/adn-facilitator \
    --account-file /tmp/fac.b64 \
    --import-keystore /tmp/adn-bench/keystore \
    > /tmp/facilitator.log 2>&1 &
"

for i in $(seq 1 30); do
  if ssh_run "$FACILITATOR_IP" "curl -s -o /dev/null -w '%{http_code}' http://localhost:7002/verify -X POST -H 'Content-Type: application/json' -d '{\"note_file_hex\":\"00\",\"merchant_id_hex\":\"0x00\"}'" 2>/dev/null | grep -q 200; then
    log "Facilitator ready."
    break
  fi
  sleep 2
done

# ─── Start merchant ─────────────────────────────────────────────────────────
log ""
log "═══ STARTING MERCHANT ═══"

# Get private IP of facilitator for internal communication
FAC_PRIVATE_IP=$(aws ec2 describe-instances --region "$REGION" --instance-ids "$FACILITATOR_ID" \
  --query 'Reservations[0].Instances[0].PrivateIpAddress' --output text)

ssh_run "$MERCHANT_IP" "
  source ~/.cargo/env
  MERCHANT_ID_HEX=\$(python3 -c \"import json; print(json.load(open('/tmp/bench-config.json'))['merchant_id_hex'])\")
  cd ~/miden-x402
  nohup ./target/release/adn-merchant \
    --port 7001 \
    --facilitator-url http://${FAC_PRIVATE_IP}:7002 \
    --merchant-id \"\$MERCHANT_ID_HEX\" \
    --settle-after $SETTLE_AFTER \
    > /tmp/merchant.log 2>&1 &
"

for i in $(seq 1 15); do
  if ssh_run "$MERCHANT_IP" "curl -s -o /dev/null -w '%{http_code}' http://localhost:7001/resource" 2>/dev/null | grep -q 402; then
    log "Merchant ready."
    break
  fi
  sleep 1
done

# ─── Get private IP of merchant for agent ────────────────────────────────────
MERCH_PRIVATE_IP=$(aws ec2 describe-instances --region "$REGION" --instance-ids "$MERCHANT_ID" \
  --query 'Reservations[0].Instances[0].PrivateIpAddress' --output text)

# ─── Run benchmark ──────────────────────────────────────────────────────────
log ""
log "═══ RUNNING BENCHMARK ($PAYMENTS payments) ═══"
log ""

BENCH_OUTPUT=$(ssh_run "$AGENT_IP" "
  source ~/.cargo/env
  cd ~/miden-x402
  ./target/release/adn-agent benchmark \
    --config /tmp/bench-config.json \
    --merchant-url http://${MERCH_PRIVATE_IP}:7001 \
    --payments $PAYMENTS \
    --amount-per-payment $AMOUNT 2>&1
")

echo "$BENCH_OUTPUT"

# ─── Print summary table ────────────────────────────────────────────────────
echo ""
echo "┌─────────────────────────────────────────────────────────────┐"
echo "│           ADN BATCH-SETTLEMENT BENCHMARK RESULTS           │"
echo "├─────────────────────────────────────────────────────────────┤"
echo "│  Infrastructure                                            │"
echo "│    Region:          $REGION                                 "
echo "│    Instance type:   $INSTANCE_TYPE                          "
echo "│    Agent:           $AGENT_IP                               "
echo "│    Merchant:        $MERCHANT_IP                            "
echo "│    Facilitator:     $FACILITATOR_IP                         "
echo "├─────────────────────────────────────────────────────────────┤"
echo "│  Configuration                                             │"
echo "│    Payments:        $PAYMENTS                               "
echo "│    Amount/payment:  $AMOUNT                                 "
echo "│    Settle every:    $SETTLE_AFTER requests                  "
echo "│    ADN balance:     $ADN_BALANCE                            "
echo "├─────────────────────────────────────────────────────────────┤"
echo "│  Results                                                   │"

# Parse results from benchmark output
THROUGHPUT=$(echo "$BENCH_OUTPUT" | grep "Throughput:" | awk '{print $2, $3}')
P50=$(echo "$BENCH_OUTPUT" | grep "p50:" | awk '{print $3}')
P95=$(echo "$BENCH_OUTPUT" | grep "p95:" | awk '{print $3}')
P99=$(echo "$BENCH_OUTPUT" | grep "p99:" | awk '{print $3}')
AVG=$(echo "$BENCH_OUTPUT" | grep "avg:" | awk '{print $3}')
TOTAL=$(echo "$BENCH_OUTPUT" | grep "Total time:" | awk '{print $3}')

printf "│    Throughput:      %-40s│\n" "${THROUGHPUT:-N/A}"
printf "│    Total time:      %-40s│\n" "${TOTAL:-N/A}"
printf "│    Latency p50:     %-40s│\n" "${P50:-N/A}"
printf "│    Latency p95:     %-40s│\n" "${P95:-N/A}"
printf "│    Latency p99:     %-40s│\n" "${P99:-N/A}"
printf "│    Latency avg:     %-40s│\n" "${AVG:-N/A}"
echo "├─────────────────────────────────────────────────────────────┤"
echo "│  Note: p50 = per-voucher (off-chain). Settlement adds      │"
echo "│  ~3-6s on-chain proving, amortized across settle_after.    │"
echo "└─────────────────────────────────────────────────────────────┘"
