#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

# Focused coverage for release_changelog_current_heading_ok (the CHANGELOG
# heading check shared by scripts/check-version-consistency.sh), plus one
# end-to-end sanity run of the real script against this repository's own
# current CHANGELOG.md/README.md. Exercises the fix that made the check
# accept both the pre-release "(unreleased)" heading and a finalized,
# strictly-dated "(YYYY-MM-DD)" heading - previously only the former was
# accepted, so a legitimately finalized release heading (as the release
# checklist itself requires at tag time) failed this gate.

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/archivefs-version-consistency-tests.XXXXXXXX")"
cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

write_changelog() {
    local path=$1
    local heading=$2
    printf '# Changelog\n\nIntro text.\n\n%s\n\nBody.\n' "$heading" >"$path"
}

expect_ok() {
    local label=$1
    local heading=$2
    local version=$3
    local changelog="$TEMP_ROOT/$label.md"
    write_changelog "$changelog" "$heading"
    release_changelog_current_heading_ok "$changelog" "$version" ||
        release_die "expected heading to be accepted ($label): $heading"
    release_note "accepted as expected: $label"
}

expect_reject() {
    local label=$1
    local heading=$2
    local version=$3
    local changelog="$TEMP_ROOT/$label.md"
    write_changelog "$changelog" "$heading"
    if release_changelog_current_heading_ok "$changelog" "$version"; then
        release_die "expected heading to be rejected ($label): $heading"
    fi
    release_note "rejected as expected: $label"
}

# The two legitimate forms.
expect_ok pre-release "## v0.8.0-alpha (unreleased)" "0.8.0-alpha"
expect_ok finalized "## v0.8.0-alpha (2026-08-18)" "0.8.0-alpha"
expect_ok finalized-leap-day "## v0.8.0-alpha (2028-02-29)" "0.8.0-alpha"

# Must still require the exact current version, not any arbitrary one.
expect_reject wrong-version-dated "## v0.8.0-alpha (2026-08-18)" "0.9.0-alpha"
expect_reject wrong-version-unreleased "## v0.7.2-alpha (unreleased)" "0.8.0-alpha"
expect_reject dot-is-not-a-wildcard "## v0X8X0-alpha (2026-08-18)" "0.8.0-alpha"

# Malformed or non-strict dates must still fail closed.
expect_reject non-strict-date "## v0.8.0-alpha (2026-8-18)" "0.8.0-alpha"
expect_reject slash-date "## v0.8.0-alpha (2026/08/18)" "0.8.0-alpha"
expect_reject trailing-text "## v0.8.0-alpha (2026-08-18) extra" "0.8.0-alpha"
expect_reject missing-parens "## v0.8.0-alpha 2026-08-18" "0.8.0-alpha"
expect_reject empty-parens "## v0.8.0-alpha ()" "0.8.0-alpha"
expect_reject unknown-word "## v0.8.0-alpha (draft)" "0.8.0-alpha"

release_note "check-version-consistency CHANGELOG-heading tests passed"

# Sanity: the real script, run against this actual repository's real
# CHANGELOG.md/README.md (no --binary-dir/--artifact/--checksum, so only
# the documentation checks run), must currently pass end to end.
"$SCRIPT_DIR/check-version-consistency.sh"
release_note "check-version-consistency.sh passes against the real repository"
