#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/compare-release-builds.sh [--output-dir DIR]

Build ArchiveFS twice with independent Cargo target and output directories,
then compare archive bytes, checksum files, payload hashes, permissions,
ownership, and timestamps.
EOF
}

REPO_ROOT="$(release_repo_root)"
OUTPUT_DIR="$REPO_ROOT/target/reproducibility"
while (($#)); do
    case "$1" in
        --output-dir)
            (($# >= 2)) || release_die "--output-dir requires a directory"
            OUTPUT_DIR=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) release_die "unknown argument: $1" ;;
    esac
done

for command in cmp python3; do
    release_require_command "$command"
done
release_require_clean_repository "$REPO_ROOT"

if [[ "$OUTPUT_DIR" != /* ]]; then
    OUTPUT_DIR="$PWD/$OUTPUT_DIR"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(CDPATH= cd -- "$OUTPUT_DIR" && pwd -P)"
[[ -z "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]] ||
    release_die "reproducibility output directory must be empty: $OUTPUT_DIR"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/archivefs-repro.XXXXXXXX")"
cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

VERSION="$(release_workspace_version "$REPO_ROOT")"
TARGET_NAME="$(release_target_name)"
BUNDLE_NAME="$(release_bundle_name "$VERSION" "$TARGET_NAME")"
SOURCE_DATE_EPOCH="$(git -C "$REPO_ROOT" log -1 --format=%ct)"
export SOURCE_DATE_EPOCH

for run in 1 2; do
    release_note "reproducibility build $run of 2"
    mkdir -p "$OUTPUT_DIR/run$run"
    "$SCRIPT_DIR/build-release.sh" \
        --output-dir "$OUTPUT_DIR/run$run" \
        --target-dir "$TEMP_ROOT/target$run"
done

ARCHIVE_ONE="$OUTPUT_DIR/run1/$BUNDLE_NAME.tar.gz"
ARCHIVE_TWO="$OUTPUT_DIR/run2/$BUNDLE_NAME.tar.gz"
CHECKSUM_ONE="$ARCHIVE_ONE.sha256"
CHECKSUM_TWO="$ARCHIVE_TWO.sha256"

manifest() {
    python3 - "$1" <<'PY'
import hashlib
import sys
import tarfile

with tarfile.open(sys.argv[1], "r:gz") as archive:
    for member in archive.getmembers():
        digest = "-"
        if member.isfile():
            stream = archive.extractfile(member)
            digest = hashlib.sha256(stream.read()).hexdigest()
        print(
            f"{member.name}\t{member.type!r}\t{member.mode & 0o7777:04o}\t"
            f"{member.uid}:{member.gid}\t{member.mtime}\t{member.size}\t{digest}"
        )
PY
}

manifest "$ARCHIVE_ONE" >"$TEMP_ROOT/manifest1"
manifest "$ARCHIVE_TWO" >"$TEMP_ROOT/manifest2"
cmp --silent "$TEMP_ROOT/manifest1" "$TEMP_ROOT/manifest2" || {
    diff -u "$TEMP_ROOT/manifest1" "$TEMP_ROOT/manifest2" >&2 || true
    release_die "extracted payload metadata or file hashes are not reproducible"
}
cmp --silent "$CHECKSUM_ONE" "$CHECKSUM_TWO" ||
    release_die "generated checksum files are not reproducible"
cmp --silent "$ARCHIVE_ONE" "$ARCHIVE_TWO" ||
    release_die "release archives are not byte-for-byte reproducible"

release_note "byte-for-byte archive reproduction verified"
release_note "payload hashes, permissions, numeric ownership, timestamps, and checksums match"
release_note "comparison outputs: $OUTPUT_DIR/run1 and $OUTPUT_DIR/run2"
