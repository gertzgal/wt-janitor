# wt-janitor

A conservative Rust CLI for managing registered Git worktrees. It discovers worktrees, detects integration natively (including squash/rebase cases), reports inactivity and disk use, removes safely integrated worktrees, and purges allowlisted reproducible dependency directories.

## Install

```sh
cargo install --path .
```

Uninstall with `cargo uninstall wt-janitor`.

## Usage

```sh
wt-janitor init
wt-janitor doctor
wt-janitor scan [--repo NAME] [--inactive-days N] [--fetch] [--json]
wt-janitor clean merged [--apply]
wt-janitor clean deps [--apply]
wt-janitor touch [PATH]
```

Configuration is read from `~/.config/wt-janitor/config.toml`; runtime state is stored at `~/.local/state/wt-janitor/state.json`. Use `--config` to select another configuration. `WT_JANITOR_STATE` overrides the state path.

## Safety and networking

Cleanup is a dry run unless `--apply` is supplied. Dry runs never remove files or worktrees. Apply performs fresh discovery and skips main, locked, detached, dirty, in-progress, uncertain, or identity-mismatched worktrees. Safe worktrees are removed with `git worktree remove` without force; detached and otherwise blocked worktrees are skipped while other safe candidates may proceed. Dependency targets are canonicalized and protected roots are refused; symlinks are removed as links only.

`--fetch` runs `git fetch --all --prune`. This is the only implicit-network Git operation, and it is never run unless explicitly requested.

Integration detection is native and conservative: ancestry, no-added-change and tree checks, a simulated merge-adds-nothing check, and patch identity are considered. Unknown results are never removal candidates.

## JSON

Stable schemas are selected by the `schema` field:

- `wt-janitor.scan/1`
- `wt-janitor.clean-merged/1`
- `wt-janitor.clean-deps/1`
- `wt-janitor.doctor/1`

The clean-merged schema retains the compatibility field `wt_prune_candidates`; it contains native minimum-age-filtered removal candidates.

## Scope

wt-janitor never creates worktrees. Independent clones that are not registered by `git worktree list` for a configured repository are invisible and out of scope.
