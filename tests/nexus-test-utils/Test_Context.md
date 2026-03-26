# Nexus Test Context

## Purpose

Use this file when you need evidence about integration coverage, reusable fixtures, or how subsystem interactions are exercised in tests.

## First Read Set

1. `Test_Summary.md`
2. `src/lib.rs`
3. one target suite such as `pipeline_tests.rs`, `multinode_tests.rs`, `rpc_integration.rs`, or `resilience_tests.rs`
4. one relevant fixture module under `fixtures/`

## Routing

- `fixtures/`: deterministic builders for crypto, primitives, network, storage, consensus-related data
- `assert_helpers.rs`: domain assertions
- `pipeline_tests.rs`: pipeline flow coverage
- `multinode_tests.rs`, `node_e2e_tests.rs`, `node_integration_tests.rs`: node-level coverage
- `rpc_integration.rs`: API coverage
- `toolchain_tests.rs`: tool CLI coverage

## Caveats

- Move-related test coverage has compile-time conditions in `src/lib.rs`; verify feature configuration before making claims.
- Some critical behaviors are tested from this crate rather than from the owning crate.
