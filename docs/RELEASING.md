# Releasing — green gates, one tag, human hands

The flow: gates → version bump → changelog → tag → release CI → artifact smoke
→ post-release checks. Everything except the pushes can be prepared by an
agent; the pushes are human work (see the two-person rule at the bottom).

## Pre-flight checklist

Before any release, confirm in order:
- [ ] Gates green (below), clean tree, version aligned (`0.N.0` ↔ milestone vN).
- [ ] LICENSE holder named (`LICENSE`).
- [ ] Every item in `docs/LEGAL.md` reviewed and signed off by the owner;
      policy drafts in `docs/*_DRAFT.md` published or explicitly deferred.
- [ ] Data-retention figures confirmed (`docs/RETENTION_DRAFT.md`).
- [ ] Channel ready for the announcement (`docs/COMMUNITY.md`). Publishing and
      channel posts are human T2 actions.

## Pre-release gates

All of these must pass. Not most of them. All.

```bash
git status                      # must be empty: clean tree, nothing untracked
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo audit                     # supply chain
cargo deny check                # licenses
```

- The toolchain is pinned in `rust-toolchain.toml` (1.99.1, clippy + rustfmt
  included). Fedora's `/usr/bin/cargo` ignores that file — if your `PATH`
  resolves to a system cargo, prefix every command: `rustup run stable cargo
  build`, or make sure rustup's cargo comes first in `PATH`.
- `make test` runs the whole suite in one target (fmt, clippy, test, audit,
  deny) — same target CI runs, so the two cannot drift.
- `make doctor` on a **scratch instance** — a fresh checkout or throwaway
  deploy, not just the dev machine. It checks binary, env, token (getMe), DB
  integrity, dashboard, service. An environment that only works where it was
  built is not a release.

## Version bump

Cargo tracks the milestone (`docs/ARCHITECTURE-V1.4.md` §5, Trust pack):
milestone vN ships as `0.N.0` — v1.4 → `0.4.0`. `0.x` means pre-1.0; minors
may break.

1. Edit `version` in `Cargo.toml`.
2. Regenerate `Cargo.lock` (`cargo check` is enough) and commit both — a
   stale lock is a broken build on the next machine.
3. `1.0.0` is gated separately: zero open P0/P1 data/money defects, security
   sign-off on `docs/SECURITY.md`, one real seller in production for 30 days.

## Tag + package

1. Finalize the changelog: rename `CHANGELOG.md`'s `[Unreleased]` to
   `[X.Y.Z] - YYYY-MM-DD`, start a fresh `[Unreleased]` above it, commit.
2. Optional local smoke of the host bundle: `make package` →
   `dist/daybook-<ver>-<platform>.<zip|tar.gz>`.
3. Tag. The release CI (`.github/workflows/release.yml`) triggers on a `v*`
   tag push and builds linux x64 + arm64, windows x64, macOS arm64 + x64, then
   creates the GitHub release with generated notes:

   ```bash
   git tag v0.4.0
   git push origin v0.4.0        # HUMAN — external publish
   ```

4. Artifact smoke: download the artifacts from the release, run one, `make
   doctor` against it, confirm the dashboard opens and the bot answers
   `/start`.

## Post-release

- Reverse-proxy operators: access-log lines containing `/setup/claim?token=`
  hold a live one-time credential — treat them as secrets (redact or drop the
  query string).
- `make backup` **before** upgrading any deployed instance — WAL-safe, backs
  up the data dir contents.
- Upgrade, then `make doctor` on the deployed instance.
- Restore drill: `make restore` on a scratch copy. Test restores quarterly; an
  untested backup is a hope, not a backup.

## The two-person rule

External publishing — GitHub push, tag push, release publish — requires
**owner approval**. It is always human-approved and human-executed. This
runbook documents the steps; it does not grant publishing authority by itself.
