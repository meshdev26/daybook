# Production sign-off record

This document records the current state of the production gate. It is not a
certification. A production claim is valid only when every item below is closed
by the named authority and the resulting evidence is linked here.

Status vocabulary: *verified* means a runnable gate was seen passing in this
session; *open* means no such evidence exists. An agent cannot close an item
that requires an external verifier or the owner.

## Verified locally and in CI

| Gate | Evidence | Status |
| --- | --- | --- |
| Build | `cargo build` succeeded on this branch | verified |
| Formatting | `cargo fmt --check` clean | verified |
| Lint | `cargo clippy --all-targets -- -D warnings` clean | verified |
| Unit tests | 804 library tests passed | verified |
| Tenant isolation | 13 integration tests passed | verified |
| Adversarial self-validation | 11 loopback tests passed | verified |
| Moderation feature | full `--features moderation` sweep passed | verified |
| Supply chain | `cargo audit` reported one pre-existing allowed warning (`rustix`); `cargo deny check licenses bans sources` passed | verified |
| PR #4 CI on `85b0cef` | all checks passed, including the fmt/clippy/test/audit/deny gate (run `46117375989`) | verified |
| PR #4 CI on `7b79fc0` | docs-only head; a fresh run (`46118653955`) was still pending at the last check | open |

## Open gates

| Gate | Required authority | Status |
| --- | --- | --- |
| Independent multi-tenant penetration test | commissioned external verifier | open |
| Scratch-deployment verification (issue #1) | human verifier on a throwaway instance | open |
| Security sign-off on `docs/SECURITY.md` | owner | open |
| One real seller in production for 30 days | owner | open |
| Version bump, new tag, release publish | owner, human-executed | open |

The code and gates above are self-validation and automated CI, not an audit.

## Owner sign-off

No owner sign-off has been recorded. The open gates must be closed by the named
authorities before a production claim can be made; their evidence belongs in the
issue tracker and release record, not in this status document.
