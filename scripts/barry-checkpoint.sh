#!/usr/bin/env bash
#
# barry-checkpoint.sh - update the factual checkpoint section of Barry's
# project-state file with automatically-collected, verifiable facts.
#
# Usage:
#   scripts/barry-checkpoint.sh
#
# Can be run from any directory - the repository root is resolved from
# this script's own location (via `git -C <script dir> rev-parse
# --show-toplevel`), never from the caller's current directory. Exits
# non-zero with a clear message if that resolved root is not the
# ArchiveFS repository (no crates/archivefs-core, or no archivefs-core
# workspace member in Cargo.toml).
#
# What it does:
#   - Creates .barry-cache/ under the repository root if it does not
#     already exist (.barry-cache/ is excluded via .git/info/exclude -
#     see that file - so this never touches tracked/staged state).
#   - Creates .barry-cache/project-state.md if it does not already exist.
#   - Collects: current UTC timestamp, branch, full and short commit SHA,
#     the workspace version from Cargo.toml, the latest local tag pointing
#     at HEAD (or "none"), `git status --short`, `git diff --stat`,
#     `git diff --cached --stat`, the most recent commit subject, and
#     whether Cargo.lock's locked archivefs-core version matches the
#     workspace version.
#   - Writes those facts as a single Markdown section, replacing only the
#     content between the two literal marker lines:
#       <!-- BARRY FACTS START -->
#       <!-- BARRY FACTS END -->
#     Every hand-written line outside those markers - anywhere earlier or
#     later in the file - is preserved byte-for-byte. If the markers are
#     missing (or malformed: not a proper start-before-end pair), the
#     facts section is appended at the end of the file instead, wrapped
#     in the same two markers, so a later run can find and replace it.
#   - Writes the result to a temporary file in the same directory as the
#     target file and replaces the target with an atomic `mv`, so a
#     reader (or a crash mid-write) never sees a half-written file.
#
# What it deliberately never does:
#   - Never modifies any source file - only .barry-cache/project-state.md.
#   - Never runs `git commit`, `git push`, or `git tag`.
#   - Never runs `cargo test`, `cargo build`, or any other build/test
#     command - every fact is read from git and Cargo.toml/Cargo.lock as
#     plain text.
#
# Tests: tests/test_barry_checkpoint.sh (run it directly, or see the
# "Run the script's tests" step in the project's normal validation flow).
# It builds a disposable, minimal fake ArchiveFS repository under a
# temporary directory (its own git history, its own Cargo.toml/Cargo.lock,
# its own copy of this script under scripts/) and drives that copy
# end-to-end - it never touches this real repository's working tree,
# history, or .barry-cache/.
set -euo pipefail

die() {
    printf 'barry-checkpoint.sh: error: %s\n' "$*" >&2
    exit 1
}

# --- Resolve the repository root from this script's own location, never
# from the caller's current directory, so `scripts/barry-checkpoint.sh`
# behaves identically whether it is run as `./scripts/barry-checkpoint.sh`
# from the repo root, as an absolute path from anywhere else, or via a
# symlink. -------------------------------------------------------------
script_source="${BASH_SOURCE[0]}"
if command -v realpath >/dev/null 2>&1; then
    script_path="$(realpath -- "$script_source")"
else
    script_path="$(cd -- "$(dirname -- "$script_source")" && pwd -P)/$(basename -- "$script_source")"
fi
script_dir="$(dirname -- "$script_path")"

repo_root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)" ||
    die "not inside a git repository (resolved from this script's location: $script_dir)"

[[ -f "$repo_root/Cargo.toml" ]] ||
    die "not inside the ArchiveFS repository: no Cargo.toml at resolved root $repo_root"
[[ -d "$repo_root/crates/archivefs-core" ]] ||
    die "not inside the ArchiveFS repository: no crates/archivefs-core under resolved root $repo_root"
grep -q 'archivefs-core' "$repo_root/Cargo.toml" ||
    die "not inside the ArchiveFS repository: Cargo.toml at $repo_root does not mention archivefs-core"

cd "$repo_root"

# --- Collect facts, each tolerant of the specific ways it can legitimately
# be absent (no tags, no staged changes, clean tree, ...) without ever
# treating that absence as a hard failure. --------------------------------

fact_timestamp="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"

fact_branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$fact_branch" == "HEAD" ]]; then
    fact_branch="HEAD (detached at $(git rev-parse --short HEAD))"
fi

fact_commit_full="$(git rev-parse HEAD)"
fact_commit_short="$(git rev-parse --short HEAD)"

fact_commit_subject="$(git log -1 --pretty=%s)"

fact_tag=""
while IFS= read -r tag_line; do
    [[ -n "$tag_line" ]] || continue
    if [[ -z "$fact_tag" ]]; then
        fact_tag="$tag_line"
    else
        fact_tag="$fact_tag, $tag_line"
    fi
done < <(git tag --points-at HEAD | sort -V)
[[ -n "$fact_tag" ]] || fact_tag="none"

fact_status="$(git status --short)"
[[ -n "$fact_status" ]] || fact_status="(clean)"

fact_diff_stat="$(git diff --stat)"
[[ -n "$fact_diff_stat" ]] || fact_diff_stat="(no unstaged changes)"

fact_staged_diff_stat="$(git diff --cached --stat)"
[[ -n "$fact_staged_diff_stat" ]] || fact_staged_diff_stat="(no staged changes)"

# Workspace version: the `version = "..."` line inside Cargo.toml's
# [workspace.package] table - not any per-crate Cargo.toml, which use
# `version.workspace = true` instead of their own literal version.
fact_workspace_version="$(
    awk '
        /^\[workspace\.package\]/ { in_section = 1; next }
        /^\[/ { in_section = 0 }
        in_section && /^version[[:space:]]*=/ {
            gsub(/^version[[:space:]]*=[[:space:]]*"|"[[:space:]]*$/, "")
            print
            exit
        }
    ' Cargo.toml
)"
[[ -n "$fact_workspace_version" ]] ||
    die "could not find a [workspace.package] version in $repo_root/Cargo.toml"

# Cargo.lock check: does the locked archivefs-core package version match
# the workspace version above? archivefs-core (like every workspace
# member) uses `version.workspace = true`, so its Cargo.lock entry's
# version is exactly the workspace version whenever the lockfile is
# current.
if [[ -f "$repo_root/Cargo.lock" ]]; then
    fact_lock_version="$(
        awk '
            /^name = "archivefs-core"$/ { found = 1; next }
            found && /^version = / {
                gsub(/^version = "|"$/, "")
                print
                exit
            }
        ' "$repo_root/Cargo.lock"
    )"
    if [[ -z "$fact_lock_version" ]]; then
        fact_lock_status="Cargo.lock has no archivefs-core entry"
    elif [[ "$fact_lock_version" == "$fact_workspace_version" ]]; then
        fact_lock_status="matches workspace version ($fact_workspace_version)"
    else
        fact_lock_status="MISMATCH - Cargo.lock has archivefs-core $fact_lock_version, workspace is $fact_workspace_version"
    fi
else
    fact_lock_status="Cargo.lock not found"
fi

# --- Render the facts block (without the surrounding markers - those are
# added by the marker-replacement/append logic below, so this same
# rendering is usable in both cases). ------------------------------------
render_facts_body() {
    cat <<FACTS
## Barry checkpoint facts

_Automatically generated by \`scripts/barry-checkpoint.sh\` - do not hand-edit this section; edit anything outside the BARRY FACTS markers instead._

- Generated: $fact_timestamp
- Branch: $fact_branch
- Commit: $fact_commit_full
- Commit (short): $fact_commit_short
- Most recent commit subject: $fact_commit_subject
- Workspace version (Cargo.toml): $fact_workspace_version
- Latest local tag at HEAD: $fact_tag
- Cargo.lock check: $fact_lock_status

### git status --short

\`\`\`
$fact_status
\`\`\`

### git diff --stat (unstaged)

\`\`\`
$fact_diff_stat
\`\`\`

### git diff --cached --stat (staged)

\`\`\`
$fact_staged_diff_stat
\`\`\`
FACTS
}

start_marker='<!-- BARRY FACTS START -->'
end_marker='<!-- BARRY FACTS END -->'

barry_dir="$repo_root/.barry-cache"
barry_file="$barry_dir/project-state.md"

mkdir -p -- "$barry_dir"

facts_tmp=""
output_tmp=""
cleanup() {
    [[ -z "$facts_tmp" ]] || rm -f -- "$facts_tmp"
    [[ -z "$output_tmp" ]] || rm -f -- "$output_tmp"
}
trap cleanup EXIT

facts_tmp="$(mktemp)"
{
    printf '%s\n' "$start_marker"
    render_facts_body
    printf '%s\n' "$end_marker"
} >"$facts_tmp"

output_tmp="$(mktemp "$barry_dir/.project-state.md.tmp.XXXXXX")"

if [[ ! -f "$barry_file" ]]; then
    # Brand new file: a minimal heading, then the facts block.
    {
        printf '# ArchiveFS Project State\n\n'
        cat -- "$facts_tmp"
    } >"$output_tmp"
elif start_line="$(grep -Fn -- "$start_marker" "$barry_file" | head -n1 | cut -d: -f1)" &&
    end_line="$(grep -Fn -- "$end_marker" "$barry_file" | head -n1 | cut -d: -f1)" &&
    [[ -n "$start_line" && -n "$end_line" ]] && ((start_line < end_line)); then
    # Existing, well-formed markers: replace only the lines between them
    # (inclusive), preserving everything before and after exactly.
    {
        head -n "$((start_line - 1))" -- "$barry_file"
        cat -- "$facts_tmp"
        tail -n "+$((end_line + 1))" -- "$barry_file"
    } >"$output_tmp"
else
    # No markers, or a malformed/reversed pair: append a fresh facts block
    # rather than guess at intent - every existing line is kept as-is.
    {
        cat -- "$barry_file"
        printf '\n'
        cat -- "$facts_tmp"
    } >"$output_tmp"
fi

mv -f -- "$output_tmp" "$barry_file"
output_tmp=""

printf 'barry-checkpoint.sh: updated %s\n' "$barry_file"
