# Benchmark Scripts

## benchmark-aws.sh

Multi-server benchmark for the miden-x402 ADN (AgentDebitNote) batch-settlement flow. Tests the full payment pipeline across three server roles:

- **Agent** -- creates an ADN note on-chain, then signs cumulative vouchers and sends them to the merchant
- **Merchant** -- runs an HTTP paywall (reference-merchant), verifies vouchers locally, and calls the facilitator's `/settle` endpoint
- **Facilitator** -- runs `/verify` and `/settle` endpoints, consumes ADN notes on Miden testnet

### Quick start (local)

Run everything on localhost -- good for development and CI:

```bash
./scripts/benchmark-aws.sh --local --payments 20
```

### Dry run

Preview all commands without executing:

```bash
./scripts/benchmark-aws.sh --local --payments 10 --dry-run
```

### AWS / multi-server

Run across three separate servers (requires SSH access):

```bash
./scripts/benchmark-aws.sh \
  --agent-host 54.1.2.3 \
  --merchant-host 54.4.5.6 \
  --facilitator-host 54.7.8.9 \
  --ssh-key ~/.ssh/miden-bench.pem \
  --ssh-user ubuntu \
  --payments 100 \
  --amount-per-payment 100
```

The script will:
1. SSH into each host, clone/pull the repo, and build release binaries
2. Run `setup-testnet --adn` on the facilitator to create accounts and the ADN note
3. Start the facilitator and merchant HTTP servers
4. Run `x402-bench --scheme adn` from the agent host
5. Collect results and print a summary

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--agent-host` | localhost | Agent server hostname/IP |
| `--merchant-host` | localhost | Merchant server hostname/IP |
| `--facilitator-host` | localhost | Facilitator server hostname/IP |
| `--payments N` | 50 | Number of voucher payments to run |
| `--amount-per-payment N` | 100 | Token amount per payment |
| `--local` | off | Run all roles on localhost (no SSH) |
| `--dry-run` | off | Print commands without executing |
| `--ssh-user` | ubuntu | SSH username for remote hosts |
| `--ssh-key` | (required for remote) | Path to SSH private key |
| `--branch` | oz-guardian-latest-flow | Git branch to clone/checkout |
| `--miden-rpc` | https://rpc.testnet.miden.io | Miden testnet RPC URL |
| `--results-dir` | ./bench-results | Where to save output |

### Environment variables

These can be used instead of CLI flags:

- `BENCH_SSH_USER` -- SSH user
- `BENCH_SSH_KEY` -- SSH key path
- `BENCH_BRANCH` -- Git branch
- `BENCH_REPO_URL` -- Repository URL
- `MIDEN_RPC` -- Miden RPC endpoint

### Output

Results are saved to `bench-results/<run-id>/` containing:

- `summary.csv` -- percentile latencies (p50/p95/p99) for each phase
- `payments.csv` -- per-payment timing breakdown
- `setup.toml` -- testnet account configuration used
- `facilitator.log` -- facilitator server logs
- `merchant.log` -- merchant server logs

The summary includes:
- Per-voucher signing time (expected ~2ms with Falcon signatures)
- Merchant-side verification time
- Facilitator /settle call latency
- End-to-end throughput (vouchers/second)

### Prerequisites

- `curl`, `jq`, `git` (all modes)
- `cargo` (local mode)
- `ssh` + SSH key (remote mode)
- Miden testnet connectivity (for `setup-testnet`)

## Other scripts

### deploy-server.sh

Provisions a single EC2 instance running the facilitator + merchant. Team members can then run `x402-bench` from their own machines against it.

### run-network-bench.sh

Provisions two EC2 instances in different AWS regions (us-east-1 and eu-west-1) to measure cross-region latency. Fully automated including infrastructure teardown.
