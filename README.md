# nexus-node

> **Version 0.1.14** — Settlement and evidence anchor node for the Nexus network.

nexus-node is a Rust workspace containing the validator node, networking, consensus, execution, intent handling, RPC services, and developer tooling.

## Architecture

```
nexus-node/
├── crates/           Core node crates (10)
│   ├── nexus-primitives   Shared types & traits
│   ├── nexus-crypto       PQC cryptography (Falcon-512, ML-DSA-65, ML-KEM-768, BLAKE3)
│   ├── nexus-network      P2P networking (libp2p + QUIC)
│   ├── nexus-storage      Persistent storage (RocksDB)
│   ├── nexus-consensus    BFT consensus engine
│   ├── nexus-execution    Move VM execution + Block-STM parallel execution
│   ├── nexus-intent       Intent pipeline
│   ├── nexus-rpc          JSON-RPC / REST / WebSocket server
│   ├── nexus-config       Configuration management
│   └── nexus-node         Binary entry point
├── tools/            Developer tools (4)
│   ├── nexus-keygen       Key generation
│   ├── nexus-genesis      Genesis file generation
│   ├── nexus-bench        Benchmarks
│   └── nexus-wallet       CLI wallet + Move package builder
├── contracts/        Smart contract examples & staking
├── tests/            Integration test utilities
├── fuzz/             Fuzz testing harnesses
└── scripts/          Devnet & operational scripts
```

## External Dependencies

| Dependency | Version | Consumption |
|-----------|---------|-------------|
| [nexus-move](https://github.com/vootaa-labs/nexus-move) | v0.1.1 | Via 4 facade crates only |

nexus-node consumes nexus-move exclusively through facade crates:
- `nexus-move-runtime` — VM execution (nexus-execution)
- `nexus-move-types` — Shared types (nexus-execution)
- `nexus-move-bytecode` — Bytecode verification (nexus-execution, conditional)
- `nexus-move-package` — Package building (nexus-wallet)

No `vendor/move-*` crates are directly depended upon.

## Developer Quick Start

```bash
# 1. Install toolchain
rustup toolchain install 1.85.0
rustup override set 1.85.0

# 2. Build
cargo build --workspace

# 3. Run tests
cargo test --workspace

# 4. Build wallet CLI
cargo build -p nexus-wallet
./target/debug/nexus-wallet move --help

# 5. Docker devnet
docker build -t nexus-node .
make devnet-setup
make devnet-up
make devnet-smoke
```

## Make Targets

```
make check          Fast workspace type-check
make lint           Format check + clippy
make test           Run workspace tests
make test-vm        Run Move VM integration tests
make security       cargo-audit + cargo-deny
make devnet         Full devnet lifecycle (build → setup → up → smoke)
make help           Show all targets
```

## Quick References

- Docs index: `Docs/README.md`
- Local rehearsal guide: `Docs/Guide/Local_Developer_Rehearsal_Guide.md`
- Devnet bootstrap: `scripts/setup-devnet.sh`
- Container smoke test: `scripts/smoke-test.sh`
- Contract smoke test: `scripts/contract-smoke-test.sh`
- Contract examples: `contracts/examples/`

## Toolchain

- Rust 1.85.0
- Resolver: edition 2021, resolver v2

## License

MIT OR Apache-2.0