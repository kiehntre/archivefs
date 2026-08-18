#!/usr/bin/env bash

# Shared, side-effect-free helpers for EmuWiz release tooling.
# This file is sourced by the executable scripts in this directory.

release_die() {
    printf 'release: error: %s\n' "$*" >&2
    exit 1
}

release_note() {
    printf 'release: %s\n' "$*"
}

release_require_command() {
    command -v "$1" >/dev/null 2>&1 || release_die "required command not found: $1"
}

release_repo_root() {
    local script_dir
    script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
    (CDPATH= cd -- "$script_dir/.." && pwd -P)
}

release_workspace_version() {
    local repo_root=$1
    (
        cd "$repo_root"
        cargo metadata --format-version 1 --no-deps |
            python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
versions = {
    package["version"]
    for package in metadata["packages"]
    if package["name"] in {"archivefs-cli", "archivefs-core", "archivefs-gui"}
}
if len(versions) != 1:
    raise SystemExit(f"EmuWiz workspace package versions disagree: {sorted(versions)}")
print(versions.pop())
'
    )
}

release_target_name() {
    case "$(uname -m)" in
        x86_64) printf '%s\n' 'x86_64-linux' ;;
        aarch64 | arm64) printf '%s\n' 'aarch64-linux' ;;
        *) release_die "unsupported release architecture: $(uname -m)" ;;
    esac
}

release_bundle_name() {
    local version=$1
    local target_name=$2
    printf 'archivefs-v%s-%s\n' "$version" "$target_name"
}

release_require_clean_repository() {
    local repo_root=$1
    local status
    status="$(git -C "$repo_root" status --porcelain=v1 --untracked-files=normal)"
    if [[ -n "$status" ]]; then
        printf '%s\n' "$status" >&2
        release_die "repository must be clean; commit, stash, or remove the paths above"
    fi
}

release_sha256() {
    sha256sum "$1" | awk '{print $1}'
}

# Whether $changelog contains the exact current-version heading for
# $version, in either of its two legitimate forms: pre-release
# ("## v$version (unreleased)") or finalized at tag time
# ("## v$version (YYYY-MM-DD)", a strict four-digit/two-digit/two-digit
# date - not a looser date shape, and never any other version). Matched as
# a whole line (anchored both ends), same as the single-form check this
# replaces.
release_changelog_current_heading_ok() {
    local changelog=$1
    local version=$2
    local escaped_version=${version//./\\.}
    grep -Eqx "## v${escaped_version} \\((unreleased|[0-9]{4}-[0-9]{2}-[0-9]{2})\\)" "$changelog"
}
