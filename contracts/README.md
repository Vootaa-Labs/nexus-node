# Nexus Move Contracts

Standard contract packages for the Nexus blockchain.

## Developer CLI

Move 合约相关操作统一通过 `nexus-wallet move ...` 执行。

```bash
cargo build -p nexus-wallet
./target/debug/nexus-wallet move --help
```

常用命令示例：

```bash
./target/debug/nexus-wallet move build --package-dir contracts/examples/counter --named-addresses counter_addr=0xCAFE --skip-fetch
./target/debug/nexus-wallet move inspect --package-dir contracts/examples/counter
```

若要执行完整的本地 devnet + 合约演练，请统一参考 `Docs/Guide/Local_Developer_Rehearsal_Guide.md`，不要在这里重复维护另一套 runbook。

## Directory Structure

```text
contracts/
├── staking/           ← Validator staking lifecycle (committee rotation)
├── examples/          ← Sample contracts (counter, token, escrow)
│   ├── counter/
│   ├── token/
│   └── escrow/
├── framework/         ← Nexus framework modules (future)
└── README.md
```

## Package Layout (per TLD-09 §4)

Every contract package follows this layout:

```text
<package-name>/
├── Move.toml          ← Package manifest
├── sources/           ← Move source files
├── scripts/           ← Transaction scripts (optional)
├── tests/             ← Move unit tests (optional)
└── Prover.toml        ← Move Prover config (optional)
```

## Build Products

When a package is compiled, the build directory contains:

```text
build/<package-name>/
├── bytecode_modules/  ← Compiled .mv files
├── package-metadata.bcs
├── source-maps/
├── abi/               ← Function ABI descriptors
└── prover-artifacts/  ← Prover verification results
```

### Nexus Artifact Bundle

Running `nexus-wallet move build` additionally generates a Nexus-specific
artifact bundle:

```text
nexus-artifact/
├── package-metadata.bcs   ← BCS-encoded PackageMetadata (Nexus format)
├── manifest.json          ← Human-readable summary
└── bytecode/
    └── <module>.mv        ← Compiled modules (copied from build/)
```

Use `nexus-wallet move inspect --package-dir <path>` to view artifact details.

## ABI Wire Format

Function ABIs are BCS-encoded as `Vec<FunctionAbi>` (see
`crates/nexus-execution/src/move_adapter/abi.rs`):

```rust
struct FunctionAbi {
    name: String,
    params: Vec<MoveType>,
    returns: Option<MoveType>,
    is_entry: bool,
}
```

## Upgrade Policy

Packages declare an upgrade policy in `Move.toml`:

| Policy | Behaviour |
| -------- | ----------- |
| `immutable` (default) | Cannot be changed after publish |
| `compatible` | ABI-compatible upgrades allowed |
| `governance` | Only governance transactions may upgrade |
