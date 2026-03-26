# Differential Test Report Template

**Report-ID**: `{date}_{invariant_id}_diff_report`  
**Invariant-ID**: `{invariant_id}`  
**Object-ID**: `{object_id}`  
**Date**: `{YYYY-MM-DD}`  
**Status**: `PASS | FAIL | PARTIAL`

## 1. Test Environment

| Item | Value |
| --- | --- |
| Rust implementation | `crates/{crate}/src/{file}` @ commit `{hash}` |
| Reference spec | `proofs/{tool}/{path}` @ version `{ver}` |
| Corpus | `proofs/differential/corpus/{corpus_file}` |
| Runner | `cargo test -p {crate} --test {test_file}` |

## 2. Scenarios Executed

| Scenario ID | Rust Result | Spec Result | Match | Notes |
| --- | --- | --- | --- | --- |
| `{scenario_1}` | `{value}` | `{value}` | ✅ / ❌ | |
| `{scenario_2}` | `{value}` | `{value}` | ✅ / ❌ | |

## 3. Invariant Checks

| Check | Expected | Actual | Pass |
| --- | --- | --- | --- |
| `{check_name}` | `{expected}` | `{actual}` | ✅ / ❌ |

## 4. Summary

- **Total scenarios**: `{N}`
- **Passed**: `{P}`
- **Failed**: `{F}`
- **Overall**: `PASS | FAIL`

## 5. Residual Risk

> (Describe any edge cases not covered, known limitations, or deferred checks.)

## 6. Reviewer Sign-off

| Role | Name | Date | Verdict |
| --- | --- | --- | --- |
| Author | | | |
| Reviewer | | | |
