# Nexus Public Testnet Release Rehearsal Runbook

> **Version:** v0.1.13  
> **Audience:** Node operators, release engineers  
> **Goal:** A single document that walks through the complete pre-release → deploy → validate → rollback cycle for a public testnet release.

---

## 0. Prerequisites

| Item | Where | Check |
|------|-------|-------|
| Rust 1.85.0 toolchain | `rust-toolchain.toml` | `rustup show` |
| Docker + Compose v2 | Host | `docker compose version` |
| `cargo-nextest` installed | CI & local | `cargo nextest --version` |
| SSH access to deployment host | Ops | `ssh -p <PORT> <HOST>` |
| GHCR write token (`CR_PAT`) | GitHub Secrets | `echo $CR_PAT \| docker login ghcr.io …` |
| `.env` or shell exports for `NEXUS_NUM_VALIDATORS` | local | default: 7 |

---

## 1. Pre-Release Verification

### 1.1 Run full CI locally

```bash
make fmt-check         # formatting gate
make clippy            # first-party lint gate, includes tests and benches
make check             # first-party type-check gate, includes tests and benches
make test-all          # nextest + doctest + KAT vectors
make devnet-smoke      # smoke-test.sh + contract-smoke-test.sh
```

Scope rule for local and CI gates:

- only first-party packages are gated
- `--all-targets` is enabled for `make clippy` and `make check`
- vendored Move and Aptos sources may build transitively, but are not treated as release-blocking diagnostics for this repo

All of the following CI gates must be green on the release branch:

| CI Job | Gate | What it covers |
|--------|------|----------------|
| `lint` | 1 | first-party formatting + clippy |
| `check` | 2 | first-party `cargo check` with tests and benches enabled |
| `security` | 3 | cargo-audit + deny advisories |
| `test` | 4a | workspace unit + lib tests |
| `correctness-negative` | 4b | Phase A–F negative-path tests |
| `startup-readiness` | 4c | readiness, lifecycle, node e2e |
| `epoch-reconfig` | 4d | epoch manager, consensus epoch, epoch store |
| `proof-surface` | 4e | proof roundtrip, commitment, snapshot signing |
| `recovery` | 5 | snapshot export/import, migration, prune |
| `coverage` | 6 | tarpaulin ≥ threshold |
| `crypto-kat` | 7 | known-answer vectors for all crypto |
| `move-vm-smoke` | 8 | Move VM compile + execute round-trip |

### 1.2 Verify soak & fault injection tests

```bash
cargo test -p nexus-test-utils --lib soak_tests       # 5 tests
cargo test -p nexus-test-utils --lib fault_injection_tests  # 20 tests
```

### 1.3 Confirm access tiers

Review `Docs/en/Ops/Testnet_Access_Policy.md` — ensure quota limits for public / developer / operator tiers match intended capacity.

---

## 2. Build Release Artifacts

### 2.1 Container image

```bash
make devnet-build   # docker build -t nexus-node:latest .
```

Or trigger the `release.yml` workflow by pushing the release tag:

```
git tag v0.1.5
git push origin v0.1.5
```

This produces multi-arch binaries (x86_64-gnu, aarch64-gnu, x86_64-musl) and uploads artifacts.

### 2.2 Tag the image for GHCR

```bash
IMAGE=ghcr.io/<owner>/nexus-node
TAG=testnet-$(git rev-parse --short HEAD)

docker tag nexus-node:latest $IMAGE:$TAG
docker tag nexus-node:latest $IMAGE:public-testnet-latest
docker push $IMAGE:$TAG
docker push $IMAGE:public-testnet-latest
```

---

## 3. Deploy to Staging

### 3.1 Bootstrap devnet (fresh deploy)

```bash
make devnet-clean            # remove old state
make devnet-setup            # generate keys + genesis for 7 validators
make devnet-up               # build + compose up -d
```

### 3.2 Deploy via CI (remote host)

Trigger the `deploy-testnet.yml` workflow manually:

| Input | Value |
|-------|-------|
| `environment` | `staging-testnet` |
| `deploy_ref` | release tag or branch |
| `compose_dir` | `/opt/nexus` |
| `healthcheck_url` | `http://127.0.0.1:8080/ready` |

The workflow will:
1. Build + push image to GHCR
2. SSH into the deployment host
3. `docker compose pull && docker compose up -d --remove-orphans`
4. Poll `/ready` (30 × 5 s = 150 s max)

### 3.3 Validate startup

```bash
scripts/validate-startup.sh -n 7 -t 90
```

Checks:
- All 7 nodes respond on `/ready`
- Peer count > 0 on at least 2f+1 nodes
- Consensus status endpoint responding

---

## 4. Post-Deploy Smoke Tests

### 4.1 Quick smoke (< 5 min)

```bash
scripts/smoke-test.sh -n 7
```

Exercises 12 scenarios:
1. Cold start readiness
2. Health / ready / metrics endpoints
3. API surface (consensus, network, validators)
4. Faucet mint + balance query
5. Single-node restart recovery
6. Late-join readiness
7. Cross-node consistency
8. Consensus round progress
9. Transaction propagation (submit node-0, query node-2)
10. Minority failure tolerance (stop f nodes)
11. Prometheus metrics format
12. Concurrent multi-node API

### 4.2 Contract smoke (< 10 min)

```bash
scripts/contract-smoke-test.sh
```

6 phases: deploy → call → query → upgrade → re-call → verify.

### 4.3 Extended soak (optional, 1–24 h)

```bash
scripts/soak-test.sh -d 1 -i 60 -n 7   # 1-hour abbreviated soak
```

Periodic checks: health, consensus advance, metrics freshness, block-height delta, tx throughput, memory stability.

---

## 5. Go / No-Go Decision

| Criterion | Pass condition |
|-----------|----------------|
| CI gates | All mandatory gates green |
| Smoke test | 12/12 scenarios pass |
| Contract smoke | 6/6 phases pass |
| Startup validation | All nodes healthy ≤ 90 s |
| Soak (if run) | No degradation alerts |
| Access policy | Quota config matches `Testnet_Access_Policy.md` |

**Go** → proceed to Step 6 (promote to public).  
**No-Go** → proceed to Step 7 (rollback).

---

## 6. Promote to Public Testnet

### 6.1 Re-tag image

```bash
docker tag $IMAGE:$TAG $IMAGE:public-testnet-latest
docker push $IMAGE:public-testnet-latest
```

### 6.2 Deploy via CI

Re-run `deploy-testnet.yml` with `environment = public-testnet`.

### 6.3 Final validation

```bash
scripts/validate-startup.sh -n 7 -t 120
scripts/smoke-test.sh -n 7
```

### 6.4 Announce

- Update `README.md` with testnet version
- Tag release in GitHub: `v0.1.5-testnet`
- Notify stakeholders

---

## 7. Rollback Procedure

### 7.1 Revert to previous image

```bash
# On the deployment host
export NEXUS_IMAGE=$IMAGE:<previous-tag>
docker compose pull
docker compose up -d --remove-orphans
```

### 7.2 Verify rollback health

```bash
scripts/validate-startup.sh -n 7 -t 120
scripts/smoke-test.sh -n 7
```

### 7.3 Local devnet rollback

```bash
make devnet-down
git checkout <previous-tag>
make devnet-up
```

### 7.4 State reset (if chain state is incompatible)

```bash
make devnet-clean     # WARNING: destroys all chain data
make devnet-up        # re-bootstrap from genesis
```

---

## 8. Post-Release Monitoring

After promoting to public testnet, monitor:

| Metric | Source | Alert threshold |
|--------|--------|-----------------|
| Node readiness | `/ready` endpoint | Any node unhealthy > 60 s |
| Consensus rounds | `/v2/consensus/status` | Stall > 2 min |
| Peer connectivity | `/v2/network/peers` | < 2f+1 connected |
| Block height delta | Cross-node query | Delta > 10 blocks |
| Memory usage | Prometheus `process_resident_memory_bytes` | Trending > 2× baseline |
| Error rate | Application logs (`nexus::audit`) | Any 5xx spike |

Use `scripts/soak-test.sh -d 24 -i 60` for continuous 24-hour validation.

---

## 9. Multi-Shard Deployment Notes

### 9.1 Genesis Generation

Multi-shard environments require the shard count to be specified in genesis:

```bash
./scripts/setup-devnet.sh -n 7 -s 2 -o devnet-n7s -f
NEXUS_NUM_SHARDS=2 ./scripts/generate-compose.sh -n 7
```

### 9.2 Upgrade Compatibility

- Upgrading from a non-sharded baseline to a multi-shard baseline requires regenerating genesis because `num_shards` is part of the durable configuration.
- Changing shard count requires state reset plus fresh genesis material. Online shard-count mutation is not supported.
- `cf_htlc_locks` is expected to be created on first startup when the backend needs it.

### 9.3 Multi-Shard Validation

After rollout, validate shard health:

```bash
# Verify shard API reachability
curl -sf http://localhost:8080/v1/shards | jq .

# Verify per-shard head state
curl -sf http://localhost:8080/v1/shards/0/head | jq .
curl -sf http://localhost:8080/v1/shards/1/head | jq .

# Verify shard count consistency across nodes
for port in 8080 8081 8082 8083; do
  echo "Node $port: $(curl -sf http://localhost:$port/v1/status | jq '.num_shards')"
done
```
