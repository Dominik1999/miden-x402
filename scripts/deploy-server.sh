#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# deploy-server.sh
#
# Provisions a single AWS EC2 instance running the x402-facilitator-server
# and reference-merchant. Once running, other team members can run the
# x402-bench (agent client) against it from any machine.
#
# Usage:
#   bash scripts/deploy-server.sh
#
# The script prints connection info and benchmark commands at the end.
# The instance stays running until you explicitly terminate it.
###############################################################################

REGION="us-east-1"
INSTANCE_TYPE="c6i.xlarge"
SSH_USER="ubuntu"
REPO_URL="https://github.com/Digine-Labs/miden-x402.git"
BRANCH="oz-guardian-latest-flow"
RUN_ID="x402-server-$(date +%Y%m%d-%H%M%S)"
KEY_NAME="miden-x402-${RUN_ID}"
SG_NAME="miden-x402-sg-${RUN_ID}"
KEY_FILE="/tmp/${KEY_NAME}.pem"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"

INSTANCE_ID=""
SG_ID=""

log() { echo "[$(date '+%H:%M:%S')] $*"; }
err() { echo "[$(date '+%H:%M:%S')] ERROR: $*" >&2; }

ssh_cmd() {
  local ip="$1"; shift
  ssh -i "$KEY_FILE" $SSH_OPTS "${SSH_USER}@${ip}" "$@"
}

scp_cmd() {
  scp -i "$KEY_FILE" $SSH_OPTS "$@"
}

find_ubuntu_ami() {
  aws ec2 describe-images \
    --region "$REGION" \
    --owners 099720109477 \
    --filters \
      "Name=name,Values=ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*" \
      "Name=state,Values=available" \
      "Name=architecture,Values=x86_64" \
    --query 'Images | sort_by(@, &CreationDate) | [-1].ImageId' \
    --output text
}

###############################################################################
# Phase 1: Provision
###############################################################################

log "============================================"
log "  x402 Server Deployment"
log "  Run ID: ${RUN_ID}"
log "============================================"
echo ""

log "Creating SSH key pair..."
aws ec2 create-key-pair \
  --region "$REGION" \
  --key-name "$KEY_NAME" \
  --key-type ed25519 \
  --query 'KeyMaterial' \
  --output text > "$KEY_FILE"
chmod 600 "$KEY_FILE"

log "Creating security group..."
SG_ID=$(aws ec2 create-security-group \
  --region "$REGION" \
  --group-name "$SG_NAME" \
  --description "miden-x402 server - ${RUN_ID}" \
  --query 'GroupId' \
  --output text)

aws ec2 authorize-security-group-ingress --region "$REGION" \
  --group-id "$SG_ID" \
  --ip-permissions \
    "IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=tcp,FromPort=7001,ToPort=7002,IpRanges=[{CidrIp=0.0.0.0/0}]" \
    "IpProtocol=icmp,FromPort=-1,ToPort=-1,IpRanges=[{CidrIp=0.0.0.0/0}]" \
  >/dev/null

log "Finding Ubuntu 24.04 AMI..."
AMI=$(find_ubuntu_ami "$REGION")
log "AMI: ${AMI}"

log "Launching instance..."
INSTANCE_ID=$(aws ec2 run-instances \
  --region "$REGION" \
  --image-id "$AMI" \
  --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --security-group-ids "$SG_ID" \
  --block-device-mappings "DeviceName=/dev/sda1,Ebs={VolumeSize=30,VolumeType=gp3}" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=miden-x402-server-${RUN_ID}}]" \
  --query 'Instances[0].InstanceId' \
  --output text)
log "Instance: ${INSTANCE_ID}"

log "Waiting for instance to start..."
aws ec2 wait instance-running --region "$REGION" --instance-ids "$INSTANCE_ID"

SERVER_IP=$(aws ec2 describe-instances \
  --region "$REGION" \
  --instance-ids "$INSTANCE_ID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' \
  --output text)
log "Server IP: ${SERVER_IP}"

log "Waiting for SSH..."
for i in $(seq 1 60); do
  if ssh_cmd "$SERVER_IP" "echo ok" &>/dev/null; then
    log "SSH ready (attempt ${i})"
    break
  fi
  if [[ $i -eq 60 ]]; then err "SSH timeout"; exit 1; fi
  sleep 5
done
echo ""

###############################################################################
# Phase 2: Install deps + build
###############################################################################

log "Installing dependencies and building (10-15 min)..."

ssh_cmd "$SERVER_IP" 'bash -s' <<'SETUP_EOF'
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

echo "=== Cloning repository ==="
git clone --depth 1 --branch oz-guardian-latest-flow https://github.com/Digine-Labs/miden-x402.git ~/miden-x402
cd ~/miden-x402

echo "=== Building release binaries ==="
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
       target/release/x402-bench
SETUP_EOF

log "Build complete."
echo ""

###############################################################################
# Phase 3: Setup testnet accounts
###############################################################################

log "Running setup-testnet (2-5 min)..."

ssh_cmd "$SERVER_IP" 'bash -lc "
  cd ~/miden-x402
  ./target/release/setup-testnet \
    --agents 1 \
    --mint-amount 1000000 \
    --out-dir ./testnet-state \
    2>&1
"' 2>&1 | sed 's/^/  /'

log "Reading configuration..."
SETUP_TOML=$(ssh_cmd "$SERVER_IP" 'cat ~/miden-x402/testnet-state/setup.toml')

MERCHANT_ID=$(echo "$SETUP_TOML" | sed -n 's/^merchant_id_hex *= *"\([^"]*\)".*/\1/p' | head -1)
FAUCET_ID=$(echo "$SETUP_TOML" | sed -n 's/^faucet_id_hex *= *"\([^"]*\)".*/\1/p' | head -1)

if [[ -z "$MERCHANT_ID" || -z "$FAUCET_ID" ]]; then
  err "Failed to extract IDs from setup.toml"
  echo "$SETUP_TOML"
  exit 1
fi

log "Merchant ID: ${MERCHANT_ID}"
log "Faucet ID:   ${FAUCET_ID}"
echo ""

###############################################################################
# Phase 4: Start services
###############################################################################

log "Starting facilitator on port 7002..."
ssh_cmd "$SERVER_IP" "bash -lc '
  cd ~/miden-x402
  mkdir -p facilitator-data
  nohup env \
    FACILITATOR_DATA_DIR=./facilitator-data \
    FACILITATOR_HTTP_PORT=7002 \
    MIDEN_RPC_ENDPOINT=https://rpc.testnet.miden.io \
    RUST_LOG=info \
    ./target/release/x402-facilitator-server \
    > facilitator.log 2>&1 &
  echo \$! > facilitator.pid
'"

for i in $(seq 1 60); do
  if ssh_cmd "$SERVER_IP" "grep -q 'listening' ~/miden-x402/facilitator.log 2>/dev/null" 2>/dev/null; then
    log "Facilitator is listening."
    break
  fi
  if [[ $i -eq 60 ]]; then
    err "Facilitator did not start. Logs:"
    ssh_cmd "$SERVER_IP" "tail -20 ~/miden-x402/facilitator.log" || true
    exit 1
  fi
  sleep 5
done

log "Starting merchant on port 7001..."
ssh_cmd "$SERVER_IP" "bash -lc '
  cd ~/miden-x402
  nohup env \
    MERCHANT_ACCOUNT_ID=${MERCHANT_ID} \
    MERCHANT_ASSET_FAUCET_ID=${FAUCET_ID} \
    MERCHANT_PRICE_AMOUNT=100 \
    MERCHANT_HTTP_PORT=7001 \
    FACILITATOR_URL=http://localhost:7002 \
    RUST_LOG=info \
    ./target/release/reference-merchant \
    > merchant.log 2>&1 &
  echo \$! > merchant.pid
'"

for i in $(seq 1 60); do
  if ssh_cmd "$SERVER_IP" "grep -q 'listening' ~/miden-x402/merchant.log 2>/dev/null" 2>/dev/null; then
    log "Merchant is listening."
    break
  fi
  if [[ $i -eq 60 ]]; then
    err "Merchant did not start. Logs:"
    ssh_cmd "$SERVER_IP" "tail -20 ~/miden-x402/merchant.log" || true
    exit 1
  fi
  sleep 5
done

echo ""

###############################################################################
# Phase 5: Download testnet-state for team
###############################################################################

log "Downloading testnet-state for team distribution..."
LOCAL_STATE_DIR="/Users/domi2000/Repos/miden-x402/testnet-state-${RUN_ID}"
mkdir -p "$LOCAL_STATE_DIR"
scp_cmd -r "${SSH_USER}@${SERVER_IP}:~/miden-x402/testnet-state/*" "${LOCAL_STATE_DIR}/"
log "Saved to: ${LOCAL_STATE_DIR}/"

echo ""
echo "============================================================"
echo "  x402 Server Running"
echo "============================================================"
echo ""
echo "  Instance:    ${INSTANCE_ID} (${REGION})"
echo "  IP:          ${SERVER_IP}"
echo "  Facilitator: http://${SERVER_IP}:7002"
echo "  Merchant:    http://${SERVER_IP}:7001"
echo "  SSH key:     ${KEY_FILE}"
echo ""
echo "  setup.toml:"
echo "$SETUP_TOML" | sed 's/^/    /'
echo ""
echo "============================================================"
echo "  Team Benchmark Instructions"
echo "============================================================"
echo ""
echo "  1. Clone and build the bench on your machine:"
echo ""
echo "     git clone --branch ${BRANCH} ${REPO_URL}"
echo "     cd miden-x402"
echo "     cargo build --release -p x402-bench"
echo ""
echo "  2. Copy testnet-state from: ${LOCAL_STATE_DIR}/"
echo "     (or download from the server):"
echo ""
echo "     scp -i ${KEY_FILE} -r ${SSH_USER}@${SERVER_IP}:~/miden-x402/testnet-state ./testnet-state"
echo ""
echo "  3. Run placeholder benchmark (no testnet needed):"
echo ""
echo "     cargo run --release -p x402-bench -- \\"
echo "       --agents 1 --payments 50 \\"
echo "       --facilitator-url http://${SERVER_IP}:7002 \\"
echo "       --merchant-url http://${SERVER_IP}:7001"
echo ""
echo "  4. Run real-Miden benchmark:"
echo ""
echo "     cargo run --release -p x402-bench -- \\"
echo "       --setup-dir ./testnet-state \\"
echo "       --miden-rpc https://rpc.testnet.miden.io \\"
echo "       --agents 1 --payments 5 \\"
echo "       --facilitator-url http://${SERVER_IP}:7002 \\"
echo "       --merchant-url http://${SERVER_IP}:7001"
echo ""
echo "  5. Quick health check:"
echo ""
echo "     curl http://${SERVER_IP}:7002/healthz"
echo "     curl http://${SERVER_IP}:7001/health"
echo ""
echo "============================================================"
echo "  Server Management"
echo "============================================================"
echo ""
echo "  SSH into server:"
echo "     ssh -i ${KEY_FILE} ${SSH_USER}@${SERVER_IP}"
echo ""
echo "  View logs:"
echo "     ssh -i ${KEY_FILE} ${SSH_USER}@${SERVER_IP} tail -f ~/miden-x402/facilitator.log"
echo "     ssh -i ${KEY_FILE} ${SSH_USER}@${SERVER_IP} tail -f ~/miden-x402/merchant.log"
echo ""
echo "  Terminate (IMPORTANT - stops billing):"
echo "     aws ec2 terminate-instances --region ${REGION} --instance-ids ${INSTANCE_ID}"
echo "     # Wait, then:"
echo "     aws ec2 delete-security-group --region ${REGION} --group-id ${SG_ID}"
echo "     aws ec2 delete-key-pair --region ${REGION} --key-name ${KEY_NAME}"
echo "     rm -f ${KEY_FILE}"
echo ""
echo "  Estimated cost: ~\$0.17/hr (\$4/day)"
echo "============================================================"
