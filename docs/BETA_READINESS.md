# Beta readiness record

This is a factual gate record. It does not declare beta, does not declare
production readiness, and does not authorize real customer use. The word "beta"
below names entry criteria only.

## 1. Scope and current status

- Version is `0.4.3-beta` (`Cargo.toml:3`), a local label only; no tag, push or
  release was made here. The beta entry criteria below remain OPEN.
- `README.md:5` calls the project beta by local label and self-reviewed (with AI assistance),
  not independently audited, and says not to use it with real customer money or
  live customer data before reading the code and testing seller isolation on
  one's own deployment. Issue #1 tracks that checklist (`README.md:7`).
- `docs/SECURITY.md:53` records that independent multi-tenant penetration
  testing has not been run; `:57` says the scratch-deployment verification in
  issue #1 remains open and the automated isolation suite is not a third-party
  audit; `:60` states alpha status, no independent audit, and no real customer
  money.
- `docs/PRODUCTION_SIGNOFF.md:1-5` says that document is not a certification and
  that a claim is valid only when every gate is closed by the named authority;
  `:38-42` records that no owner sign-off exists.
- Current work is on `security/local-adversarial`, ten commits ahead of
  `master` and tracked against `origin/security/local-adversarial`; PR #4 to
  `master` is OPEN (`git status --short --branch`; `git log --oneline master..HEAD`;
  `gh pr view 4 --json state,headRefName,baseRefName`).

## 2. Beta entry criteria and current status

| Criterion | Required authority and evidence | Status | Evidence pointer |
| --- | --- | --- | --- |
| Independent penetration test with a report | Commissioned external verifier; report linked as evidence | OPEN - no test run, no report recorded | `docs/SECURITY.md:53`, `:57`; `docs/PRODUCTION_SIGNOFF.md:30` (external verifier, open) |
| Scratch-deployment verification / issue #1 | Human verifier on a throwaway instance; `make doctor` there | OPEN - issue #1 is OPEN | `docs/SECURITY.md:57`; `README.md:7`; `docs/RELEASING.md:38-40`; `Makefile:52-54` (`doctor` = read-only environment diagnostics); `gh issue view 1 --json state` |
| Owner security sign-off on `docs/SECURITY.md` | Owner; recorded sign-off | OPEN - none recorded | `docs/PRODUCTION_SIGNOFF.md:32`, `:38-42` |
| Defined pilot scope (e.g. non-money or limited sellers), with monitoring and rollback | Owner; bounded scope, observability, and a tested rollback path | OPEN - no pilot scope recorded; one real seller for 30 days is still an open gate | `docs/PRODUCTION_SIGNOFF.md:26-35`, `:33`; `docs/SECURITY.md:60`; `AGENT.md:95-97` (`RUST_LOG` observability); section 4 below and `docs/RELEASING.md:79-83` |
| Version bump and owner-executed tag / release | Owner, human-approved and human-executed external publish | OPEN - local label is `0.4.3-beta`; no tag or release was made here | `Cargo.toml:3`; `docs/RELEASING.md:43-53`, `:61-68`, `:85-89`; `docs/PRODUCTION_SIGNOFF.md:34` |

`docs/RELEASING.md:52-53` separately gates 1.0 on zero open P0/P1 data or money
defects, security sign-off, and one real seller in production for 30 days; those
are not claimed here. `docs/RELEASING.md:85-89` requires owner approval and human
execution for GitHub push, tag push, and release publish, and says the runbook
does not grant publishing authority.

## 3. Verified local and CI gates (not an audit)

`docs/PRODUCTION_SIGNOFF.md:13-24` records these as verified on earlier heads:
build, formatting, clippy, 810 library tests, 13 isolation tests, 13 loopback
adversarial tests, the full `moderation` feature sweep, `cargo audit` with one
allowed pre-existing `rustix` warning, `cargo deny check licenses bans sources`,
and PR #4 CI run `46117375989` on `85b0cef`.

These are self-validation and automated CI, not an audit
(`docs/PRODUCTION_SIGNOFF.md:36`; `docs/SECURITY.md:53`, `:55-57`). No
independent report is linked in the open-gate table
(`docs/PRODUCTION_SIGNOFF.md:26-35`).

CI status changes with every push and must be read from GitHub Actions rather
than from any document (`docs/PRODUCTION_SIGNOFF.md:24`). At the time of this
record, the latest run for the current head `08867ba` is `46118930062`,
conclusion `failure`; the failing job is "Gate (fmt, clippy, test, audit,
deny)" (`gh run list --branch security/local-adversarial --limit 8 --json
databaseId,headSha,conclusion,url`; `gh run view 46118930062 --json jobs,conclusion`).
Earlier runs on earlier heads were successful, as recorded at
`docs/PRODUCTION_SIGNOFF.md:23-24`; this record does not convert them into a
pass for the current head.

## 4. Rollback record

- Rollback operates on this branch's commits, not on master:
  `git log --oneline master..HEAD` lists `08867ba`, `f443e01`, `7b79fc0`,
  `85b0cef`, `ea5d1af`, `a1bc05e`, `7f36fde`.
- `master` remains at `c456419`, the same commit as `origin/master`
  (`git branch -vv`; `git rev-parse HEAD master`). No master commit was made by
  this work. PR #4 is still open against `master`.
- A local pre-tenant-scope checkpoint exists: tag `checkpoint/pre-tenant-scope`
  resolves to `194bbec`, "chore(release): align version to 0.4.3-alpha and
  document security work" (`git tag --list`; `git rev-parse
  checkpoint/pre-tenant-scope^{commit}`; `git log -1 --format=%s 194bbec`). It is
  an ancestor of the current head (`git merge-base --is-ancestor
  checkpoint/pre-tenant-scope HEAD` returned 0) and is not a release tag;
  release tags follow the `v*` convention in `docs/RELEASING.md:61-68`.
- Recorded rollback sequence: stop deploying the branch; for a throwaway
  deployment, check out the pre-tenant-scope checkpoint or revert the branch
  commits; leave `master` at `c456419`. Back up a deployed instance before
  changing it (`make backup`, `docs/RELEASING.md:79-80`), run `make doctor`
  after upgrade (`docs/RELEASING.md:81`; `Makefile:52-54`), and use `make
  restore` on a scratch copy as the restore drill (`docs/RELEASING.md:82-83`).

## 5. What an agent cannot do

An agent cannot commission an independent penetration test
(`docs/PRODUCTION_SIGNOFF.md:30`), cannot touch production or move real customer
money (`README.md:5`; `docs/SECURITY.md:60`), cannot sign off security
(`docs/PRODUCTION_SIGNOFF.md:32`, `:38-42`), and cannot bump the version, push a
tag, or publish a release without the owner (`docs/RELEASING.md:49-53`, `:67`,
`:85-89`; `docs/PRODUCTION_SIGNOFF.md:34`). No tag, release, sign-off,
or beta-readiness claim was made by this record; the local version label was
changed separately.

This record does not declare beta readiness; the named gates remain open.
