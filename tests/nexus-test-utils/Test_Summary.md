# Nexus Test Summary

## Scope

`nexus-test-utils` is both a fixture crate and a home for integration-style suites that exercise cross-crate behavior.

## Module Map

| Module | Current responsibility |
| --- | --- |
| `lib.rs` | crate exports and test module registration |
| `fixtures/` | reusable builders and deterministic test data |
| `assert_helpers.rs` | domain-specific assertions |
| `tracing_init.rs` | tracing setup for tests |
| `pipeline_tests.rs` | transaction pipeline coverage |
| `multinode_tests.rs` | multi-validator behavior |
| `network_integration_tests.rs` | networking scenarios |
| `node_e2e_tests.rs` and `node_integration_tests.rs` | node lifecycle and integration coverage |
| `resilience_tests.rs` | restart and recovery-style scenarios |
| `rpc_integration.rs` | API behavior |
| `toolchain_tests.rs` | keygen, genesis, Move tooling behavior |

## Important Facts

- Crate root forbids unsafe code.
- Many cross-crate behavior claims should be checked here before editing runtime code.
- Move integration test compilation is conditional in `src/lib.rs`.

## Minimal Read Paths

- Fixture question: `src/lib.rs` → `fixtures/` target module
- System test question: `src/lib.rs` → target suite
