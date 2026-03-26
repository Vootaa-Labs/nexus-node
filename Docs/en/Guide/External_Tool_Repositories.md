# External Tool Repositories

## Overview

Two developer tools — `nexus-simulator` and `nexus-monitor` — were part of the original monolithic Nexus workspace but are **not included** in the `nexus-node` v0.1.13 repository. They remain in separate repositories as auxiliary tooling.

## Tools

### nexus-simulator

- **Role**: Scenario simulation for development and testing. Provides scripted transaction replay and network behavior simulation.
- **Scope**: Development-time tool. Not required for node operation or CI.
- **Status**: External repository. Not a dependency of `nexus-node`.

### nexus-monitor

- **Role**: Terminal UI (TUI) for real-time operational monitoring. Tracks consensus readiness, node `/ready` status, and SLO metrics.
- **Scope**: Operations/observability tool. Referenced in `Ops/Testnet_SLO.md` for readiness failure detection.
- **Status**: External repository. Not a dependency of `nexus-node`.

## Integration Boundary

- `nexus-node` does **not** declare Cargo dependencies on either tool.
- Both tools consume `nexus-node`'s RPC endpoints (REST, WebSocket) as external clients.
- Operational runbooks may reference `nexus-monitor` for monitoring procedures.
- Neither tool is required for building, testing, or deploying `nexus-node`.
