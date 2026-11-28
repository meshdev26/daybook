# Verus proof plan (draft)

This is a draft plan, not a verification result. No formal verification has been
performed, no verifier is installed, and no dependency or gate was changed.

## 1. Target and contract

The candidate is `src/handlers/money.rs::classify_transfer_error` (`money.rs:20-71`).
It is a pure `&str -> TransferOutcome` function, so it is the smallest useful
proof target in the money path.

The contract to prove is an implication: `DefiniteRejected` is returned only for
an HTTP status in 400..=499 other than 408, or for a normalized string in the
recognized definite token set. Empty input, transport errors, 408, 5xx,
unparseable HTTP tokens and unrecognized strings must stay `Ambiguous`.

Non-goals: this proof would not cover the DB refund guard, quantization helpers,
webhook signature verification, owner approval, persist-then-call idempotency,
caller handling of the outcome, or whether real provider errors match the list.

## 2. Toolchain proposal

No verifier exists in the tree: `verus` is not installed, and no source or config
mentions Verus. A repeatable setup would pin a Verus version and a compatible
rustc, and record both here. No `Cargo.toml` default-build change and no tool
installation happen in this draft; both require owner approval and the project
gate.

## 3. Annotation surface

A proof would need a spec function and explicit `external_body` treatment for
standard-library operations the verifier does not model directly: `trim`,
`parse::<u16>`, `replace`, `format!`, prefix checks and iterator searching. Each
external body weakens the claim and must be listed in the proof record.

A second candidate is `money.rs::minor_units` (`money.rs:307`) and its separate
copy in `src/db.rs:5747`. The two quantizers are not identical, so proving one
does not cover the other; a proof of either must name the exact function.

## 4. Repeatability

A controlled environment would add a verifier entrypoint and a CI job that
reports the verifier version and fails on a proof error. Both are drafted here
only; no Makefile or workflow was changed.

## 5. Scope boundaries

No release gate, version, tag or documentation claim changes until a proof is
actually checked. The existing proptest and table-driven tests remain the
evidence in force.

## Acceptance

Acceptance requires gate-sanctioned approval of the toolchain, a controlled
environment with Verus installed, a zero-admission proof of section 1 for that
one function, and a record of the verifier version and exact command. Until then
this document stays a draft and no proof is claimed.
