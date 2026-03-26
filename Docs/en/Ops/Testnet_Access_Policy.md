# Nexus Public Testnet — Access & Abuse Policy

_Version 0.1.13_

---

## 1. Overview

This document defines the rules governing public access to the Nexus
devnet/testnet RPC endpoints, including rate limits, authentication tiers,
faucet policy, and abuse response procedures.

---

## 2. Access Tiers

| Tier | Identification | Description |
|------|---------------|-------------|
| **Anonymous** | No `x-api-key` header | Default for all callers. Lowest quotas. |
| **Authenticated** | Valid `x-api-key` header | Registered developers. Mid-tier quotas. |
| **Whitelisted** | `x-api-key` in the whitelisted set | Trusted partners / auditors. Highest quotas. |

API keys must be at least 16 bytes and are transmitted in the `x-api-key`
HTTP header. TLS is required when API keys are configured (enforced at
startup).

---

## 3. Rate Limits

### 3.1 Global per-IP limit

All endpoints share a blanket per-IP rate limit:

| Parameter | Default |
|-----------|---------|
| Per-IP RPS | 100 |
| Window | 1 second |

Exceeding the global limit returns **429 Too Many Requests** with a
`retry-after` header.

### 3.2 Per-endpoint-class quotas (E-2)

Compute-intensive endpoints have separate per-class, per-tier limits
(requests per minute):

| Endpoint Class | Paths | Anonymous | Authenticated | Whitelisted |
|---------------|-------|-----------|---------------|-------------|
| **Query** | `/v2/contract/query` | 60 rpm | 600 rpm | 3 000 rpm |
| **Intent** | `/v2/intent/submit`, `/v2/intent/estimate-gas` | 30 rpm | 300 rpm | 1 500 rpm |
| **MCP** | `/v2/mcp/*` | 30 rpm | 300 rpm | 1 500 rpm |

Each class tracks limits **independently** — exhausting your query quota
does not affect intent or MCP quotas.

Response headers on quota-protected endpoints:

- `x-quota-tier` — resolved caller tier
- `x-quota-class` — endpoint class (query / intent / mcp)
- `x-quota-remaining` — tokens left in the current window

### 3.3 Query gas budget

Read-only view queries (`/v2/contract/query`) are subject to a gas budget:

| Parameter | Default |
|-----------|---------|
| Gas budget | 10 000 000 units |
| Timeout | 5 000 ms |

Queries exceeding the budget or timeout are rejected with **400** or
**503** respectively. The response includes `gas_used` and `gas_budget`
fields for observability.

---

## 4. Faucet Policy

The `/v2/faucet/mint` endpoint dispenses testnet tokens for development.

| Parameter | Default |
|-----------|---------|
| Enabled | `false` (must be explicitly enabled) |
| Amount per request | 10⁹ voo (1 NXS) |
| Per-address limit | 10 requests per hour |

- Each unique address is tracked independently.
- When the address tracking table reaches capacity (100 000 entries),
  requests from new addresses are rejected (**fail-closed**) until
  existing entries expire.

---

## 5. Audit Logging

All requests are logged to structured JSON under the `nexus::audit` tracing
target. Fields captured:

| Field | Description |
|-------|-------------|
| `method` | HTTP method |
| `path` | Request URI path |
| `status` | HTTP response status code |
| `latency_ms` | End-to-end latency |
| `ip` | Client IP (from TCP peer, not `X-Forwarded-For`) |
| `request_id` | Unique `x-request-id` header |
| `tier` | Resolved quota tier |
| `endpoint_class` | Endpoint class (query / intent / mcp) |

Query-specific gas metrics (`gas_used`, `gas_budget`, `elapsed_ms`) are
additionally logged by the contract query handler.

---

## 6. Abuse Detection & Response

### 6.1 Automated protections

- **Rate limiting** — per-IP token-bucket with fail-closed overflow.
- **Quota tiering** — compute-intensive endpoints have tighter anonymous
  limits.
- **Gas budget** — prevents expensive view queries from consuming
  unbounded resources.
- **Timeout** — long-running queries are killed after the configured
  deadline.
- **Body size limit** — 512 KiB max request body.
- **API key authentication** — mutating endpoints (POST) require a valid
  key when configured.

### 6.2 Manual response procedures

When automated protections are insufficient:

1. **Identify** — check audit logs (`nexus::audit`) for anomalous
   patterns: high request rates, repeated 429s from rotating IPs,
   gas-budget violations.
2. **Block** — add offending IPs at the reverse-proxy / firewall level
   (not in the application) for immediate effect.
3. **Revoke** — remove compromised API keys from the `api_keys`
   configuration and restart the node.
4. **Escalate** — if the attack targets protocol-level resources (e.g.
   storage amplification), engage the core team to evaluate deeper
   mitigations.

### 6.3 Post-incident review

After an abuse event:

- Archive audit logs covering the incident window.
- Evaluate whether automated limits need tightening.
- Update this document if new attack vectors were discovered.

---

## 7. Configuration Reference

All parameters are in `nexus-config` → `RpcConfig`. They can be set via
the node configuration file or environment overrides.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `rate_limit_per_ip_rps` | u32 | 100 | Global per-IP RPS |
| `query_rate_limit_anonymous_rpm` | u32 | 60 | Query class, anonymous |
| `query_rate_limit_authenticated_rpm` | u32 | 600 | Query class, authenticated |
| `query_rate_limit_whitelisted_rpm` | u32 | 3 000 | Query class, whitelisted |
| `intent_rate_limit_anonymous_rpm` | u32 | 30 | Intent class, anonymous |
| `intent_rate_limit_authenticated_rpm` | u32 | 300 | Intent class, authenticated |
| `intent_rate_limit_whitelisted_rpm` | u32 | 1 500 | Intent class, whitelisted |
| `mcp_rate_limit_anonymous_rpm` | u32 | 30 | MCP class, anonymous |
| `mcp_rate_limit_authenticated_rpm` | u32 | 300 | MCP class, authenticated |
| `mcp_rate_limit_whitelisted_rpm` | u32 | 1 500 | MCP class, whitelisted |
| `query_gas_budget` | u64 | 10 000 000 | Max gas per view query |
| `query_timeout_ms` | u64 | 5 000 | View query timeout |
| `faucet_enabled` | bool | false | Enable faucet endpoint |
| `faucet_amount` | u64 | 10⁹ voo | Tokens per faucet request |
| `faucet_per_addr_limit_per_hour` | u32 | 10 | Faucet per-address hourly limit |
| `api_keys` | Vec | [] | Valid API keys (min 16 bytes each) |
| `whitelisted_api_keys` | Vec | [] | Whitelisted-tier keys (subset of api_keys) |
| `cors_allowed_origins` | Vec | [] | Fail-closed when empty |
