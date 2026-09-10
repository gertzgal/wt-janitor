#!/usr/bin/env bash
# Build a scratch scenario repo exercising wt-janitor end-to-end (dry-run only).
set -euo pipefail
ROOT=${1:-/tmp/wt-janitor-demo}
rm -rf "$ROOT"
mkdir -p "$ROOT"
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid

REPO="$ROOT/main-repo"
git init -q -b main "$REPO"
cd "$REPO"
echo hello > file.txt && git add . && git commit -qm "init"

# Worktree with a normally-merged branch (integrated: ancestor)
git worktree add -q "$ROOT/merged-wt" -b feature/normal
cd "$ROOT/merged-wt"
echo change > f2.txt && git add . && git commit -qm "feature work"
cd "$REPO" && git merge -q --no-ff feature/normal -m "merge feature/normal"

# Squash-merged branch (Worktrunk should detect: trees match / patch-id)
git worktree add -q "$ROOT/squash-wt" -b feature/squash
cd "$ROOT/squash-wt"
echo squash > s.txt && git add . && git commit -qm "squash work"
cd "$REPO"
git merge -q --squash feature/squash && git commit -qm "squash feature/squash"

# Unmerged stale branch with a dependency dir (old commit time)
cd "$REPO"
git worktree add -q "$ROOT/stale-wt" -b feature/stale
cd "$ROOT/stale-wt"
mkdir -p node_modules/pkg pkg/node_modules .venv/bin
echo fake > node_modules/pkg/index.js
echo fake > pkg/node_modules/x.js
echo dep > .venv/bin/activate
git add -A && git commit -qm "stale feature"
touch -t 202401010000 file.txt f2.txt 2>/dev/null || true
# backdate the branch tip so it looks inactive
GIT_COMMITTER_DATE="2024-01-01T00:00:00" GIT_AUTHOR_DATE="2024-01-01T00:00:00" \
  git commit -q --amend --no-edit --date="2024-01-01T00:00:00"
# Age every meaningful source file so the activity heuristic sees inactivity
# (checkout timestamps would otherwise always look "fresh").
find . -type f \
  -not -path './.git/*' -not -name .git \
  -not -path './node_modules/*' -not -path './.venv/*' -not -path './pkg/node_modules/*' \
  -exec touch -t 202401010000 {} +

# Locked worktree, integrated
cd "$REPO"
git worktree add -q "$ROOT/locked-wt" -b feature/locked
cd "$ROOT/locked-wt" && echo x > l.txt && git add . && git commit -qm "locked work"
cd "$REPO" && git merge -q feature/locked
git worktree lock "$ROOT/locked-wt"

# Detached worktree
cd "$REPO"
git worktree add -q --detach "$ROOT/detached-wt"

# Dirty integrated worktree (untracked file)
cd "$REPO"
git worktree add -q "$ROOT/dirty-wt" -b feature/dirty
cd "$ROOT/dirty-wt" && echo d > d.txt && git add . && git commit -qm "dirty work"
cd "$REPO" && git merge -q feature/dirty
echo untracked > "$ROOT/dirty-wt/scratch.txt"

# Worktree path with spaces
cd "$REPO"
git worktree add -q "$ROOT/dir with spaces/space-wt" -b feature/spaces
cd "$ROOT/dir with spaces/space-wt" && echo s > sp.txt && git add . && git commit -qm "spaces work"
cd "$REPO" && git merge -q feature/spaces

# Fresh active worktree (recent commit)
cd "$REPO"
git worktree add -q "$ROOT/active-wt" -b feature/active
cd "$ROOT/active-wt" && echo a > a.txt && git add . && git commit -qm "active work"

echo "SCENARIO READY: $REPO"
