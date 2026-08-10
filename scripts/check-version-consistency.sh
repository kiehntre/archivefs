#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/check-version-consistency.sh [--binary-dir DIR] [--artifact FILE] [--checksum FILE]

Check the Cargo workspace, current release documentation, built binaries,
and optional artifact filenames against one workspace version.
EOF
}

BINARY_DIR=""
ARTIFACT=""
CHECKSUM=""
while (($#)); do
    case "$1" in
        --binary-dir)
            (($# >= 2)) || release_die "--binary-dir requires a directory"
            BINARY_DIR=$2
            shift 2
            ;;
        --artifact)
            (($# >= 2)) || release_die "--artifact requires a file"
            ARTIFACT=$2
            shift 2
            ;;
        --checksum)
            (($# >= 2)) || release_die "--checksum requires a file"
            CHECKSUM=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) release_die "unknown argument: $1" ;;
    esac
done

REPO_ROOT="$(release_repo_root)"
VERSION="$(release_workspace_version "$REPO_ROOT")"
TARGET_NAME="$(release_target_name)"
BUNDLE_NAME="$(release_bundle_name "$VERSION" "$TARGET_NAME")"

grep -Fqx "## v$VERSION (unreleased)" "$REPO_ROOT/CHANGELOG.md" ||
    release_die "CHANGELOG.md must contain the current heading: ## v$VERSION (unreleased)"
grep -Fq "v$VERSION" "$REPO_ROOT/README.md" ||
    release_die "README.md does not mention current release v$VERSION"

if [[ -n "$BINARY_DIR" ]]; then
    [[ -x "$BINARY_DIR/emuwiz-cli" ]] || release_die "CLI binary missing in $BINARY_DIR"
    [[ -x "$BINARY_DIR/emuwiz" ]] || release_die "GUI binary missing in $BINARY_DIR"
    [[ "$($BINARY_DIR/emuwiz-cli --version)" == "emuwiz-cli $VERSION" ]] ||
        release_die "CLI --version disagrees with workspace version"
    [[ "$($BINARY_DIR/emuwiz --version)" == "emuwiz $VERSION" ]] ||
        release_die "GUI --version disagrees with workspace version"
fi

if [[ -n "$ARTIFACT" ]]; then
    [[ "$(basename -- "$ARTIFACT")" == "$BUNDLE_NAME.tar.gz" ]] ||
        release_die "artifact filename disagrees with workspace version"
fi
if [[ -n "$CHECKSUM" ]]; then
    [[ "$(basename -- "$CHECKSUM")" == "$BUNDLE_NAME.tar.gz.sha256" ]] ||
        release_die "checksum filename disagrees with workspace version"
fi

release_note "version consistency verified: $VERSION"
