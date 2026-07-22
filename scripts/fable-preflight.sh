#!/usr/bin/env bash
# Safe, non-destructive precondition checker for the Fable/ArchiveFS GUI
# integration campaign. Never installs, mutates, mounts, unmounts, scans
# the real library, touches RetroArch config, or performs network access.
set -euo pipefail

EXPECTED_DIR="/home/davedap/archivefs-fable"
EXPECTED_BRANCH="fable-archivefs-integration"
EXPECTED_BASE_COMMIT="fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6"
PROTECTED_PATHS=(
    "/home/davedap/archivefs"
    "/home/davedap/archivefs-codex"
)

pass_count=0
fail_count=0

pass() {
    printf 'PASS: %s\n' "$1"
    pass_count=$((pass_count + 1))
}

fail() {
    printf 'FAIL: %s\n' "$1"
    fail_count=$((fail_count + 1))
}

# 1. Exact expected repository path.
actual_dir="$(pwd -P)"
if [ "$actual_dir" = "$EXPECTED_DIR" ]; then
    pass "working directory is $EXPECTED_DIR"
else
    fail "working directory is '$actual_dir', expected '$EXPECTED_DIR'"
fi

# 2. Expected branch.
current_branch="$(git branch --show-current 2>/dev/null || true)"
if [ "$current_branch" = "$EXPECTED_BRANCH" ]; then
    pass "branch is $EXPECTED_BRANCH"
else
    fail "branch is '$current_branch', expected '$EXPECTED_BRANCH'"
fi

# 3. Expected campaign base ancestry.
head_commit="$(git rev-parse HEAD 2>/dev/null || true)"
if [ "$head_commit" = "$EXPECTED_BASE_COMMIT" ]; then
    pass "HEAD is exactly the campaign base commit ($EXPECTED_BASE_COMMIT)"
elif git merge-base --is-ancestor "$EXPECTED_BASE_COMMIT" HEAD 2>/dev/null; then
    pass "HEAD ($head_commit) descends from campaign base commit ($EXPECTED_BASE_COMMIT)"
else
    fail "HEAD ($head_commit) is not the campaign base commit and does not descend from it"
fi

# 4. Clean tracked state.
if git diff --quiet HEAD -- . 2>/dev/null && [ -z "$(git status --porcelain --untracked-files=no)" ]; then
    pass "tracked working tree is clean"
else
    fail "tracked working tree has uncommitted changes"
fi

# 5-9. No merge/rebase/cherry-pick/revert/bisect in progress.
git_dir="$(git rev-parse --git-dir 2>/dev/null || true)"
if [ -n "$git_dir" ]; then
    if [ -e "$git_dir/MERGE_HEAD" ]; then
        fail "a merge is in progress (MERGE_HEAD present)"
    else
        pass "no merge in progress"
    fi

    if [ -d "$git_dir/rebase-merge" ] || [ -d "$git_dir/rebase-apply" ]; then
        fail "a rebase is in progress"
    else
        pass "no rebase in progress"
    fi

    if [ -e "$git_dir/CHERRY_PICK_HEAD" ]; then
        fail "a cherry-pick is in progress (CHERRY_PICK_HEAD present)"
    else
        pass "no cherry-pick in progress"
    fi

    if [ -e "$git_dir/REVERT_HEAD" ]; then
        fail "a revert is in progress (REVERT_HEAD present)"
    else
        pass "no revert in progress"
    fi

    if [ -e "$git_dir/BISECT_LOG" ]; then
        fail "a bisect is in progress (BISECT_LOG present)"
    else
        pass "no bisect in progress"
    fi
else
    fail "could not determine git directory"
fi

# 10. Protected paths are outside this worktree.
for protected in "${PROTECTED_PATHS[@]}"; do
    case "$protected" in
        "$EXPECTED_DIR"|"$EXPECTED_DIR"/*)
            fail "protected path '$protected' is inside this worktree"
            ;;
        *)
            pass "protected path '$protected' is outside this worktree"
            ;;
    esac
done

# 11. Design reference exists.
if [ -e "design-reference" ]; then
    pass "design-reference exists"
else
    fail "design-reference is missing"
fi

# 12. Design reference is ignored by Git.
if [ -e "design-reference" ]; then
    if git check-ignore -q "design-reference" 2>/dev/null; then
        pass "design-reference is ignored by Git"
    else
        fail "design-reference is NOT ignored by Git"
    fi
fi

# 13/14. Rust toolchain and Cargo exist.
if command -v rustc >/dev/null 2>&1; then
    pass "rustc is available ($(rustc --version 2>/dev/null))"
else
    fail "rustc is not available"
fi

if command -v cargo >/dev/null 2>&1; then
    pass "cargo is available ($(cargo --version 2>/dev/null))"
else
    fail "cargo is not available"
fi

# 15. Required local commands exist.
for cmd in git python3; do
    if command -v "$cmd" >/dev/null 2>&1; then
        pass "required command '$cmd' is available"
    else
        fail "required command '$cmd' is missing"
    fi
done

# 16. cargo metadata succeeds (local only, no network fetch of new deps
# expected since Cargo.lock is not modified by this script).
if cargo metadata --no-deps --format-version 1 >/dev/null 2>&1; then
    pass "cargo metadata succeeds"
else
    fail "cargo metadata failed"
fi

# 17. Current HEAD (informational, always passes if readable).
if [ -n "$head_commit" ]; then
    pass "current HEAD is $head_commit"
else
    fail "could not read current HEAD"
fi

# 18/19. origin/main relationship; no accidental divergence from origin/main.
origin_main="$(git rev-parse origin/main 2>/dev/null || true)"
if [ -n "$origin_main" ]; then
    pass "origin/main resolves to $origin_main"
    if [ "$head_commit" = "$origin_main" ] || git merge-base --is-ancestor "$origin_main" HEAD 2>/dev/null; then
        pass "HEAD is origin/main or a descendant of it (no accidental divergence)"
    else
        fail "HEAD is not origin/main and does not descend from it"
    fi
else
    fail "could not resolve origin/main"
fi

# 20. No tracked files under design-reference.
tracked_design_files="$(git ls-files -- design-reference 2>/dev/null || true)"
if [ -z "$tracked_design_files" ]; then
    pass "no tracked files under design-reference"
else
    fail "tracked files exist under design-reference: $tracked_design_files"
fi

echo
echo "----------------------------------------"
echo "fable-preflight: $pass_count passed, $fail_count failed"
echo "----------------------------------------"

if [ "$fail_count" -gt 0 ]; then
    exit 1
fi

exit 0
