# Proof And Snapshot Trust Model

## 1. Overview

This report explains the trust boundary for state commitment, proof, and
snapshot-style verification surfaces in the current Nexus baseline.

## 2. Trust Assumptions

### 2.1 What the client must trust

A client still needs trusted bootstrap material such as:

- correct genesis or trusted initial state root
- validator-set information appropriate for the relevant epoch window
- the security of the hash and signature primitives in use

### 2.2 What the client does not need to trust blindly

With a correct proof model, the client does not need to trust:

- the honesty of a single node serving the response
- an unauthenticated transport channel for the integrity of the proof payload itself

### 2.3 Rotation-related trust boundary

As validator sets evolve with epoch and committee changes, client-side proof
verification still depends on having the right validator context for the relevant
window.

### 2.4 Multi-shard proof boundary

Once shard-aware state and observation become operationally relevant, proof
discussion has to include both local proof validity and global interpretation of
the state view.

### 2.5 Current limitations

The existence of proof routes does not, by itself, solve client bootstrap,
validator-set distribution, or external wallet trust decisions.

## 3. API Surfaces

### 3.1 State commitment query

The current baseline exposes state-commitment-related routes as part of the real
API surface. That means commitment visibility is a current feature claim, not a
future placeholder.

### 3.2 Single-key proof

Single-key proof behavior should be described as a concrete client-verification
surface, not merely as an internal data-structure detail.

### 3.3 Batch or higher-level proof surfaces

Where multiple proof-related routes exist, operators and integrators should
distinguish structure, trust bootstrap, and performance expectations explicitly.

## 4. Client Verification Flow

### 4.1 Inclusion proof verification

The client should verify that the provided proof structure resolves to the
expected commitment root.

### 4.2 Exclusion proof verification

Exclusion proofs are part of the trust model as well. They matter because an API
that can only prove presence but not absence is weaker than the current baseline
actually claims.

### 4.3 Best practices

Integrators should:

- tie proof verification to the expected root and epoch context
- keep bootstrap material explicit and reviewable
- avoid treating transport success as proof validity

## 5. Snapshot Integrity Validation

### 5.1 Snapshot structure

Snapshot-oriented verification should be documented as a composition of state,
root, and signing context rather than as an opaque blob exchange.

### 5.2 Signing and verification

Snapshot signing belongs to the same trust discussion as proof serving: both are
security-relevant release surfaces.

### 5.3 Offline verification

Offline verification remains useful because it removes runtime trust in the
serving node from the integrity check itself.

### 5.4 Tamper detection

The practical value of the proof model is that tampering should be detectable by
structure and root mismatch, not by operator promise alone.

## 6. Security Boundary Summary

The current public baseline includes state commitment and proof-related routes in
the node-facing API surface. That means proof verification should be documented
as an active part of the product surface, not as a future-only concept.

## 7. Current Baseline Vs Mainnet Ambition

At `v0.1.13`, the right public claim is that Nexus exposes a proof-oriented trust
boundary for clients, while the exact client-side verification pipeline still
depends on the integrator's trust bootstrap and validation strategy.

## Appendix: Operator And Integrator Guidance

- verify proof structure against the expected root
- tie validator or epoch context to the proof-verification flow
- treat snapshot signing and proof serving as security-relevant release surfaces
