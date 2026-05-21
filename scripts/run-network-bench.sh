#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# run-network-bench.sh
#
# Provisions two AWS EC2 instances (us-east-1 and eu-west-1), builds the
# miden-x402 project on both, runs the x402-facilitator-server + reference-
# merchant on Instance A, and runs x402-bench from Instance B to measure
# real cross-region network latency.
###############################################################################

REGION_SERVER="us-east-1"
REGION_AGENT="eu-west-1"
INSTANCE_TYPE="c6i.xlarge"
SSH_USER="ubuntu"
REPO_URL="https://github.com/Digine-Labs/miden-x402.git"
BRANCH="oz-guardian-latest-flow"
RUN_ID="bench-$(date +%Y%m%d-%H%M%S)"
KEY_NAME="miden-bench-${RUN_ID}"
SG_NAME_SERVER="miden-bench-sg-server-${RUN_ID}"
SG_NAME_AGENT="miden-bench-sg-agent-${RUN_ID}"
KEY_FILE="/tmp/${KEY_NAME}.pem"
LOCAL_RESULTS_DIR="/Users/domi2000/Repos/miden-x402/bench-results"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"

# Track resources for cleanup
INSTANCE_ID_SERVER=""
INSTANCE_ID_AGENT=""
SG_ID_SERVER=""
SG_ID_AGENT=""
KEY_CREATED_SERVER=false
KEY_CREATED_AGENT=false

###############################################################################
# Helpers
###############################################################################

log() {
  echo "[$(date '+%H:%M:%S')] $*"
}

err() {
  echo "[$(date '+%H:%M:%S')] ERROR: $*" >&2
}

ssh_cmd() {
  local ip="$1"; shift
  ssh -i "$KEY_FILE" $SSH_OPTS "${SSH_USER}@${ip}" "$@"
}

scp_cmd() {
  scp -i "$KEY_FILE" $SSH_OPTS "$@"
}

wait_for_ssh() {
  local ip="$1"
  local max_attempts=60
  log "Waiting for SSH on ${ip}..."
  for i in $(seq 1 $max_attempts); do
    if ssh_cmd "$ip" "echo ok" &>/dev/null; then
      log "SSH ready on ${ip} (attempt ${i})"
      return 0
    fi
    sleep 5
  done
  err "SSH not ready on ${ip} after ${max_attempts} attempts"
  return 1
}

find_ubuntu_ami() {
  local region="$1"
  aws ec2 describe-images \
    --region "$region" \
    --owners 099720109477 \
    --filters \
      "Name=name,Values=ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*" \
      "Name=state,Values=available" \
      "Name=architecture,Values=x86_64" \
    --query 'Images | sort_by(@, &CreationDate) | [-1].ImageId' \
    --output text
}

###############################################################################
# Cleanup handler
###############################################################################

cleanup() {
  local exit_code=$?
  echo ""
  log "=== Cleanup ==="

  if [[ -n "$INSTANCE_ID_SERVER" ]]; then
    log "Terminating server instance ${INSTANCE_ID_SERVER} in ${REGION_SERVER}..."
    aws ec2 terminate-instances --region "$REGION_SERVER" \
      --instance-ids "$INSTANCE_ID_SERVER" &>/dev/null || true
  fi

  if [[ -n "$INSTANCE_ID_AGENT" ]]; then
    log "Terminating agent instance ${INSTANCE_ID_AGENT} in ${REGION_AGENT}..."
    aws ec2 terminate-instances --region "$REGION_AGENT" \
      --instance-ids "$INSTANCE_ID_AGENT" &>/dev/null || true
  fi

  # Wait for instances to terminate before deleting security groups
  if [[ -n "$INSTANCE_ID_SERVER" ]]; then
    log "Waiting for server instance to terminate..."
    aws ec2 wait instance-terminated --region "$REGION_SERVER" \
      --instance-ids "$INSTANCE_ID_SERVER" &>/dev/null || true
  fi
  if [[ -n "$INSTANCE_ID_AGENT" ]]; then
    log "Waiting for agent instance to terminate..."
    aws ec2 wait instance-terminated --region "$REGION_AGENT" \
      --instance-ids "$INSTANCE_ID_AGENT" &>/dev/null || true
  fi

  if [[ -n "$SG_ID_SERVER" ]]; then
    log "Deleting security group ${SG_ID_SERVER} in ${REGION_SERVER}..."
    aws ec2 delete-security-group --region "$REGION_SERVER" \
      --group-id "$SG_ID_SERVER" &>/dev/null || true
  fi

  if [[ -n "$SG_ID_AGENT" ]]; then
    log "Deleting security group ${SG_ID_AGENT} in ${REGION_AGENT}..."
    aws ec2 delete-security-group --region "$REGION_AGENT" \
      --group-id "$SG_ID_AGENT" &>/dev/null || true
  fi

  if [[ "$KEY_CREATED_SERVER" == true ]]; then
    log "Deleting key pair in ${REGION_SERVER}..."
    aws ec2 delete-key-pair --region "$REGION_SERVER" \
      --key-name "$KEY_NAME" &>/dev/null || true
  fi

  if [[ "$KEY_CREATED_AGENT" == true ]]; then
    log "Deleting key pair in ${REGION_AGENT}..."
    aws ec2 delete-key-pair --region "$REGION_AGENT" \
      --key-name "$KEY_NAME" &>/dev/null || true
  fi

  if [[ -f "$KEY_FILE" ]]; then
    rm -f "$KEY_FILE"
  fi

  log "Cleanup complete (exit code: ${exit_code})"
}

trap cleanup EXIT

###############################################################################
# Phase 1: Provision
###############################################################################

log "============================================"
log "  miden-x402 Network Benchmark"
log "  Run ID: ${RUN_ID}"
log "============================================"
echo ""

# --- Key pair ---
log "Phase 1: Provisioning infrastructure..."
log "Creating SSH key pair '${KEY_NAME}'..."

aws ec2 create-key-pair \
  --region "$REGION_SERVER" \
  --key-name "$KEY_NAME" \
  --key-type ed25519 \
  --query 'KeyMaterial' \
  --output text > "$KEY_FILE"
chmod 600 "$KEY_FILE"
KEY_CREATED_SERVER=true

# Import the same key into the agent region
aws ec2 import-key-pair \
  --region "$REGION_AGENT" \
  --key-name "$KEY_NAME" \
  --public-key-material fileb://<(ssh-keygen -y -f "$KEY_FILE")
KEY_CREATED_AGENT=true

log "Key pair created and imported."

# --- Security groups ---
log "Creating security group in ${REGION_SERVER}..."
SG_ID_SERVER=$(aws ec2 create-security-group \
  --region "$REGION_SERVER" \
  --group-name "$SG_NAME_SERVER" \
  --description "miden-x402 bench server - ${RUN_ID}" \
  --query 'GroupId' \
  --output text)

aws ec2 authorize-security-group-ingress --region "$REGION_SERVER" \
  --group-id "$SG_ID_SERVER" \
  --ip-permissions \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=tcp,FromPort=7001,ToPort=7002,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=icmp,FromPort=-1,ToPort=-1,IpRanges=[{CidrIp=0.0.0.0/0}]" \
  >/dev/null

log "Server security group: ${SG_ID_SERVER}"

log "Creating security group in ${REGION_AGENT}..."
SG_ID_AGENT=$(aws ec2 create-security-group \
  --region "$REGION_AGENT" \
  --group-name "$SG_NAME_AGENT" \
  --description "miden-x402 bench agent - ${RUN_ID}" \
  --query 'GroupId' \
  --output text)

aws ec2 authorize-security-group-ingress --region "$REGION_AGENT" \
  --group-id "$SG_ID_AGENT" \
  --ip-permissions \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=0.0.0.0/0}]" \
  >/dev/null

log "Agent security group: ${SG_ID_AGENT}"

# --- Find AMIs ---
log "Finding latest Ubuntu 24.04 AMI in ${REGION_SERVER}..."
AMI_SERVER=$(find_ubuntu_ami "$REGION_SERVER")
log "Server AMI: ${AMI_SERVER}"

log "Finding latest Ubuntu 24.04 AMI in ${REGION_AGENT}..."
AMI_AGENT=$(find_ubuntu_ami "$REGION_AGENT")
log "Agent AMI: ${AMI_AGENT}"

# --- Launch instances ---
log "Launching server instance in ${REGION_SERVER}..."
INSTANCE_ID_SERVER=$(aws ec2 run-instances \
  --region "$REGION_SERVER" \
  --image-id "$AMI_SERVER" \
  --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --security-group-ids "$SG_ID_SERVER" \
  --block-device-mappings "DeviceName=/dev/sda1,Ebs={VolumeSize=30,VolumeType=gp3}" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=miden-bench-server-${RUN_ID}}]" \
  --query 'Instances[0].InstanceId' \
  --output text)
log "Server instance: ${INSTANCE_ID_SERVER}"

log "Launching agent instance in ${REGION_AGENT}..."
INSTANCE_ID_AGENT=$(aws ec2 run-instances \
  --region "$REGION_AGENT" \
  --image-id "$AMI_AGENT" \
  --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --security-group-ids "$SG_ID_AGENT" \
  --block-device-mappings "DeviceName=/dev/sda1,Ebs={VolumeSize=30,VolumeType=gp3}" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=miden-bench-agent-${RUN_ID}}]" \
  --query 'Instances[0].InstanceId' \
  --output text)
log "Agent instance: ${INSTANCE_ID_AGENT}"

# --- Wait for running ---
log "Waiting for instances to enter 'running' state..."
aws ec2 wait instance-running --region "$REGION_SERVER" --instance-ids "$INSTANCE_ID_SERVER" &
aws ec2 wait instance-running --region "$REGION_AGENT" --instance-ids "$INSTANCE_ID_AGENT" &
wait
log "Both instances are running."

# --- Get public IPs ---
SERVER_IP=$(aws ec2 describe-instances \
  --region "$REGION_SERVER" \
  --instance-ids "$INSTANCE_ID_SERVER" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' \
  --output text)

AGENT_IP=$(aws ec2 describe-instances \
  --region "$REGION_AGENT" \
  --instance-ids "$INSTANCE_ID_AGENT" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' \
  --output text)

log "Server IP (${REGION_SERVER}): ${SERVER_IP}"
log "Agent IP  (${REGION_AGENT}): ${AGENT_IP}"
log "Estimated cross-region RTT (us-east-1 <-> eu-west-1): ~70-90ms"
echo ""

# --- Wait for SSH ---
wait_for_ssh "$SERVER_IP" &
wait_for_ssh "$AGENT_IP" &
wait
log "SSH ready on both instances."
echo ""

###############################################################################
# Phase 2: Setup (parallel on both instances)
###############################################################################

log "Phase 2: Installing dependencies and building (this takes 10-20 min)..."

SETUP_SCRIPT='#!/bin/bash
set -euo pipefail

echo "=== Installing system dependencies ==="
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  build-essential pkg-config libssl-dev cmake git \
  libpq-dev protobuf-compiler curl

echo "=== Installing Rust 1.93.0 ==="
curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.93.0
. "$HOME/.cargo/env"
rustc --version
cargo --version

echo "=== Cloning repository ==="
git clone --depth 1 --branch '"$BRANCH"' '"$REPO_URL"' ~/miden-x402
cd ~/miden-x402

echo "=== Building release binaries ==="
echo "This will take a while..."
cargo build --release \
  -p setup-testnet \
  -p reference-merchant \
  -p x402-facilitator-server \
  -p x402-bench \
  2>&1 | tail -5

echo "=== Build complete ==="
ls -la target/release/setup-testnet \
       target/release/reference-merchant \
       target/release/x402-facilitator-server \
       target/release/x402-bench 2>/dev/null || echo "Some binaries missing!"
'

log "Starting setup on server (${SERVER_IP})..."
ssh_cmd "$SERVER_IP" "$SETUP_SCRIPT" 2>&1 | sed 's/^/  [server] /' &
PID_SERVER_SETUP=$!

log "Starting setup on agent (${AGENT_IP})..."
ssh_cmd "$AGENT_IP" "$SETUP_SCRIPT" 2>&1 | sed 's/^/  [agent]  /' &
PID_AGENT_SETUP=$!

log "Waiting for both builds to complete..."
wait $PID_SERVER_SETUP
log "Server build finished."
wait $PID_AGENT_SETUP
log "Agent build finished."
echo ""

###############################################################################
# Phase 3: Run servers on Instance A
###############################################################################

log "Phase 3: Setting up testnet accounts and starting servers..."

# Run setup-testnet
log "Running setup-testnet on server (this takes 2-5 min)..."
ssh_cmd "$SERVER_IP" 'bash -lc "
  cd ~/miden-x402
  mkdir -p testnet-state
  ./target/release/setup-testnet \
    --agents 1 \
    --mint-amount 1000000 \
    --out-dir ./testnet-state \
    2>&1
"' 2>&1 | sed 's/^/  [setup] /'

log "setup-testnet complete. Reading configuration..."

# Extract merchant and faucet IDs from setup.toml
SETUP_TOML=$(ssh_cmd "$SERVER_IP" 'cat ~/miden-x402/testnet-state/setup.toml')
echo "  setup.toml contents:"
echo "$SETUP_TOML" | sed 's/^/    /'

MERCHANT_ID=$(echo "$SETUP_TOML" | sed -n 's/^merchant_id_hex *= *"\([^"]*\)".*/\1/p' | head -1)
FAUCET_ID=$(echo "$SETUP_TOML" | sed -n 's/^faucet_id_hex *= *"\([^"]*\)".*/\1/p' | head -1)

if [[ -z "$MERCHANT_ID" || -z "$FAUCET_ID" ]]; then
  err "Failed to extract merchant_id_hex or faucet_id_hex from setup.toml"
  err "setup.toml contents:"
  echo "$SETUP_TOML"
  exit 1
fi

log "Merchant ID: ${MERCHANT_ID}"
log "Faucet ID:   ${FAUCET_ID}"

# Start facilitator server
log "Starting x402-facilitator-server on port 7002..."
ssh_cmd "$SERVER_IP" "bash -lc '
  cd ~/miden-x402
  mkdir -p facilitator-data
  nohup env \
    FACILITATOR_DATA_DIR=./facilitator-data \
    FACILITATOR_HTTP_PORT=7002 \
    MIDEN_RPC_ENDPOINT=https://rpc.testnet.miden.io \
    ./target/release/x402-facilitator-server \
    > facilitator.log 2>&1 &
  echo \$! > facilitator.pid
  echo \"Facilitator PID: \$(cat facilitator.pid)\"
'"

# Wait for facilitator to be ready
log "Waiting for facilitator to become ready..."
for i in $(seq 1 60); do
  if ssh_cmd "$SERVER_IP" "grep -q 'listening' ~/miden-x402/facilitator.log 2>/dev/null" 2>/dev/null; then
    log "Facilitator is listening."
    break
  fi
  if [[ $i -eq 60 ]]; then
    err "Facilitator did not become ready in time. Last log lines:"
    ssh_cmd "$SERVER_IP" "tail -20 ~/miden-x402/facilitator.log" || true
    exit 1
  fi
  sleep 5
done

# Start reference merchant
log "Starting reference-merchant on port 7001..."
ssh_cmd "$SERVER_IP" "bash -lc '
  cd ~/miden-x402
  nohup env \
    MERCHANT_ACCOUNT_ID=${MERCHANT_ID} \
    MERCHANT_ASSET_FAUCET_ID=${FAUCET_ID} \
    MERCHANT_PRICE_AMOUNT=100 \
    MERCHANT_HTTP_PORT=7001 \
    FACILITATOR_URL=http://localhost:7002 \
    ./target/release/reference-merchant \
    > merchant.log 2>&1 &
  echo \$! > merchant.pid
  echo \"Merchant PID: \$(cat merchant.pid)\"
'"

# Wait for merchant to be ready
log "Waiting for merchant to become ready..."
for i in $(seq 1 60); do
  if ssh_cmd "$SERVER_IP" "grep -q 'listening' ~/miden-x402/merchant.log 2>/dev/null" 2>/dev/null; then
    log "Merchant is listening."
    break
  fi
  if [[ $i -eq 60 ]]; then
    err "Merchant did not become ready in time. Last log lines:"
    ssh_cmd "$SERVER_IP" "tail -20 ~/miden-x402/merchant.log" || true
    exit 1
  fi
  sleep 5
done

log "Both servers are running on Instance A."
echo ""

###############################################################################
# Phase 4: Copy setup artifacts to Instance B
###############################################################################

log "Phase 4: Copying testnet-state from server to agent..."

# Download from server, then upload to agent
TMPDIR_STATE=$(mktemp -d)
scp_cmd -r "${SSH_USER}@${SERVER_IP}:~/miden-x402/testnet-state" "${TMPDIR_STATE}/"
scp_cmd -r "${TMPDIR_STATE}/testnet-state" "${SSH_USER}@${AGENT_IP}:~/miden-x402/"
rm -rf "$TMPDIR_STATE"

log "testnet-state copied to agent."
echo ""

###############################################################################
# Phase 5: Run benchmark from Instance B
###############################################################################

log "Phase 5: Running benchmarks from ${REGION_AGENT} -> ${REGION_SERVER}..."
echo ""

# Measure RTT
log "--- Measuring RTT (ping) ---"
ssh_cmd "$AGENT_IP" "ping -c 10 ${SERVER_IP}" 2>&1 | sed 's/^/  /' | tee /tmp/miden-bench-rtt.txt
echo ""

# Placeholder mode benchmark (50 payments)
log "--- Running placeholder-mode benchmark (50 payments) ---"
ssh_cmd "$AGENT_IP" "bash -lc '
  cd ~/miden-x402
  ./target/release/x402-bench \
    --agents 1 \
    --payments 50 \
    --facilitator-url http://${SERVER_IP}:7002 \
    --merchant-url http://${SERVER_IP}:7001 \
    2>&1
'" 2>&1 | sed 's/^/  [placeholder] /'
echo ""

# Real-Miden mode benchmark (5 payments)
log "--- Running real-Miden benchmark (5 payments) ---"
ssh_cmd "$AGENT_IP" "bash -lc '
  cd ~/miden-x402
  ./target/release/x402-bench \
    --setup-dir ./testnet-state \
    --miden-rpc https://rpc.testnet.miden.io \
    --agents 1 \
    --payments 5 \
    --facilitator-url http://${SERVER_IP}:7002 \
    --merchant-url http://${SERVER_IP}:7001 \
    2>&1
'" 2>&1 | sed 's/^/  [real-miden] /'
echo ""

###############################################################################
# Phase 6: Collect results
###############################################################################

log "Phase 6: Collecting results..."

mkdir -p "$LOCAL_RESULTS_DIR"

# Try to SCP bench-out directory from agent
scp_cmd -r "${SSH_USER}@${AGENT_IP}:~/miden-x402/bench-out" \
  "${LOCAL_RESULTS_DIR}/" 2>/dev/null || {
    log "No bench-out directory found on agent (bench may output differently)."
}

# Also grab server logs for reference
scp_cmd "${SSH_USER}@${SERVER_IP}:~/miden-x402/facilitator.log" \
  "${LOCAL_RESULTS_DIR}/facilitator.log" 2>/dev/null || true
scp_cmd "${SSH_USER}@${SERVER_IP}:~/miden-x402/merchant.log" \
  "${LOCAL_RESULTS_DIR}/merchant.log" 2>/dev/null || true

# Copy RTT measurement
cp /tmp/miden-bench-rtt.txt "${LOCAL_RESULTS_DIR}/rtt.txt" 2>/dev/null || true

echo ""
log "=== Results ==="
echo ""

# Print summary.csv — it's under bench-out/run-<timestamp>/summary.csv
SUMMARY_FILE=$(find "${LOCAL_RESULTS_DIR}/bench-out/" -name "summary.csv" -type f 2>/dev/null | sort | tail -1)
if [[ -n "$SUMMARY_FILE" ]]; then
  log "summary.csv ($(basename $(dirname "$SUMMARY_FILE"))):"
  cat "$SUMMARY_FILE"
  echo ""
  # Also print the payments.csv for detail
  PAYMENTS_FILE="$(dirname "$SUMMARY_FILE")/payments.csv"
  if [[ -f "$PAYMENTS_FILE" ]]; then
    log "payments.csv:"
    cat "$PAYMENTS_FILE"
    echo ""
  fi
else
  log "No summary.csv found. Listing bench-out/ contents:"
  find "${LOCAL_RESULTS_DIR}/bench-out/" -type f 2>/dev/null | sed 's/^/  /' || true
fi

# Print RTT summary
log "RTT measurement:"
grep -E "^(rtt|PING|---)" "${LOCAL_RESULTS_DIR}/rtt.txt" 2>/dev/null | sed 's/^/  /' || true
echo ""

log "Results saved to: ${LOCAL_RESULTS_DIR}/"
echo ""

###############################################################################
# Phase 7: Cleanup prompt
###############################################################################

echo "============================================"
echo "  Benchmark complete!"
echo "  Server: ${INSTANCE_ID_SERVER} (${SERVER_IP}) in ${REGION_SERVER}"
echo "  Agent:  ${INSTANCE_ID_AGENT} (${AGENT_IP}) in ${REGION_AGENT}"
echo "============================================"
echo ""
echo "Terminate instances and clean up resources? [y/N]"
read -r CONFIRM

if [[ "$CONFIRM" =~ ^[Yy]$ ]]; then
  log "Cleaning up resources..."
  # cleanup will run via the EXIT trap
  exit 0
else
  log "Skipping cleanup. Resources are still running!"
  log "To clean up manually:"
  echo "  aws ec2 terminate-instances --region ${REGION_SERVER} --instance-ids ${INSTANCE_ID_SERVER}"
  echo "  aws ec2 terminate-instances --region ${REGION_AGENT} --instance-ids ${INSTANCE_ID_AGENT}"
  echo "  # Wait for termination, then:"
  echo "  aws ec2 delete-security-group --region ${REGION_SERVER} --group-id ${SG_ID_SERVER}"
  echo "  aws ec2 delete-security-group --region ${REGION_AGENT} --group-id ${SG_ID_AGENT}"
  echo "  aws ec2 delete-key-pair --region ${REGION_SERVER} --key-name ${KEY_NAME}"
  echo "  aws ec2 delete-key-pair --region ${REGION_AGENT} --key-name ${KEY_NAME}"
  echo "  rm -f ${KEY_FILE}"
  # Prevent trap from cleaning up
  INSTANCE_ID_SERVER=""
  INSTANCE_ID_AGENT=""
  SG_ID_SERVER=""
  SG_ID_AGENT=""
  KEY_CREATED_SERVER=false
  KEY_CREATED_AGENT=false
  exit 0
fi
