# Working on wt-janitor

wt-janitor is a single Rust crate and binary for conservative Git-worktree cleanup. Preserve safety and stable CLI behavior before optimizing or simplifying implementation.

## Start here

1. Read `README.md` for the user contract and safety model.
2. Find the relevant module below; follow existing types and vocabulary.
3. Make the smallest coherent change.
4. Add or update tests using temporary repositories only.
5. Run `make check`. Work is complete when formatting, strict Clippy, and the full suite pass.

## Developer experience

```sh
cargo run -- --help                 # run the development binary
cargo run -- scan --config FILE     # exercise a command against an explicit config
cargo test                          # full suite; warm run should stay under 10 seconds
cargo test --test integration NAME  # focused real-Git scenario
cargo fmt                           # format
cargo clippy --all-targets -- -D warnings
make check                          # format + lint + test
cargo install --path . --force      # install the local binary
```

Tests require `git`; they require no Python, Worktrunk, network, or real user repositories. `tests/common/mod.rs` builds isolated repositories and worktrees under `tempfile` directories. Extend those helpers rather than hand-rolling fixtures. `tests/scenario.sh` is a manual scratch-fixture generator.

The repository may be supplied without its own `.git` directory. Do not assume Git history is available.

## Architecture

- `src/cli.rs`: clap surface, command orchestration, exit-code policy, state-path environment override.
- `src/discovery.rs`: one-repository scan and recommendation policy.
- `src/git/porcelain.rs`: NUL-safe worktree discovery parser.
- `src/git/internals.rs`: repository metadata through gix; explicit fetch helper.
- `src/git/integration.rs`: conservative native integration checks.
- `src/git/status.rs`: dirty-state classification, including untracked files.
- `src/cleanup.rs`: merged-worktree and dependency plans/apply flows.
- `src/deps.rs`: deletion-time path validation and protected-root enforcement.
- `src/activity.rs`, `src/sizes.rs`, `src/state.rs`: inactivity, budgeted disk walking, atomic state.
- `src/report.rs`: terminal rendering and stable JSON payloads.
- `src/progress.rs`: stderr progress; stdout remains machine-readable for `--json`.
- `src/doctor.rs`: Git, configuration, repository, and state checks.
- `src/models.rs`: fixed action and integration vocabulary.
- `src/error.rs`: exit codes 0–4 and typed failures.

`src/lib.rs` exposes modules for integration tests; `src/main.rs` only maps CLI results to process exits.

## Non-negotiable invariants

### Process boundary

Rust and gix perform all repository logic. The only external executable is `git`. Pass argument arrays through `src/proc.rs`; every process has a timeout and never invokes a shell.

Allowed Git subprocesses:

- discovery: `git worktree list --porcelain -z`
- explicit `--fetch`: `git fetch --all --prune`
- apply merged: `git worktree remove -- PATH`, without force

Use gix for other Git metadata and reference operations. Progress belongs on stderr so JSON stdout remains valid.

### Destructive behavior

Cleanup is dry-run by default; only `--apply` deletes. Apply performs fresh discovery and never trusts an earlier report. Keep main, locked, detached, dirty, in-progress, missing, identity-mismatched, and unknown-integration worktrees out of removal candidates. Never use force deletion or `git worktree prune` to remove live worktrees.

Dependency deletion canonicalizes immediately before deletion. A target must remain strictly below its worktree and must not be `/`, `$HOME`, the repository root, or the worktree root. Remove symlinks as links without following their targets. Use Rust filesystem APIs, never shell deletion.

State writes remain atomic: private temporary file, file fsync, rename, directory fsync. First-seen state shields newly discovered worktrees.

### Compatibility

Keep commands, flags, exit codes, action names, integration reason strings, and JSON schemas stable. Schema IDs are:

- `wt-janitor.scan/1`
- `wt-janitor.clean-merged/1`
- `wt-janitor.clean-deps/1`
- `wt-janitor.doctor/1`

The clean-merged JSON field `wt_prune_candidates` is retained for compatibility despite native removal. Additive JSON changes require care; renaming or removing fields requires an explicit schema version change.

`--inactive-days 0` means “use the configured value.” `WT_JANITOR_STATE` overrides the default state path. Repository selection preserves repeated `--repo` order.

## Integration semantics

Integration checks run cheapest-first and stop at the first match:

1. same commit
2. branch is an ancestor of target
3. merge-base-to-branch adds no changes
4. tree IDs match
5. simulated merge adds nothing to target
6. matching blob-change identity in at most 500 target commits

Reason strings are `same-commit`, `ancestor`, `no-added-changes`, `trees-match`, `merge-adds-nothing`, and `patch-id-match`. A completed search with no match is `not-integrated`; a failed or indeterminate check is `unknown`. Unknown is always conservative and never removable. Simulated merge objects stay in an in-memory object database so scans do not mutate the repository.

## Coding and test conventions

Lean on type inference and existing domain structs. Avoid explicit return types where inference is practical; never erase type safety to bypass a compiler error. Keep walkers budgeted and symlink-safe.

Every behavior change needs the narrowest relevant test plus regression coverage for safety-sensitive paths. CLI tests invoke `env!("CARGO_BIN_EXE_wt-janitor")`; JSON tests parse stdout rather than matching formatting. Real-Git tests use only temporary paths and configure author/committer identity locally. Never point tests or manual apply commands at a user's existing repository.
