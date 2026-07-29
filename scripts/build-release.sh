#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/build-release.sh [--output-dir DIR] [--target-dir DIR]

Build and verify the canonical ArchiveFS Linux release archive.

Options:
  --output-dir DIR  Destination for the archive and .sha256 file.
                    Default: target/release-artifacts
  --target-dir DIR  Cargo target directory. Useful for clean reproducibility runs.
                    Default: Cargo's normal target directory.
  -h, --help        Show this help.
EOF
}

REPO_ROOT="$(release_repo_root)"
OUTPUT_DIR="$REPO_ROOT/target/release-artifacts"
TARGET_DIR=""

while (($#)); do
    case "$1" in
        --output-dir)
            (($# >= 2)) || release_die "--output-dir requires a directory"
            OUTPUT_DIR=$2
            shift 2
            ;;
        --target-dir)
            (($# >= 2)) || release_die "--target-dir requires a directory"
            TARGET_DIR=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) release_die "unknown argument: $1" ;;
    esac
done

for command in cargo git gzip install python3 sha256sum strings tar; do
    release_require_command "$command"
done

[[ "$(git -C "$REPO_ROOT" rev-parse --show-toplevel)" == "$REPO_ROOT" ]] ||
    release_die "script must belong to the repository worktree root"
release_require_clean_repository "$REPO_ROOT"

VERSION="$(release_workspace_version "$REPO_ROOT")"
TARGET_NAME="$(release_target_name)"
BUNDLE_NAME="$(release_bundle_name "$VERSION" "$TARGET_NAME")"

if [[ "$OUTPUT_DIR" != /* ]]; then
    OUTPUT_DIR="$PWD/$OUTPUT_DIR"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(CDPATH= cd -- "$OUTPUT_DIR" && pwd -P)"

ARTIFACT="$OUTPUT_DIR/$BUNDLE_NAME.tar.gz"
CHECKSUM="$ARTIFACT.sha256"
[[ ! -e "$ARTIFACT" && ! -e "$CHECKSUM" ]] ||
    release_die "output already exists; choose an empty output directory: $OUTPUT_DIR"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/archivefs-release.XXXXXXXX")"
cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

STAGE_ROOT="$TEMP_ROOT/$BUNDLE_NAME"
mkdir -p "$STAGE_ROOT"

SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$REPO_ROOT" log -1 --format=%ct)}"
[[ "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] || release_die "SOURCE_DATE_EPOCH must be an integer"
export SOURCE_DATE_EPOCH
export LC_ALL=C
export TZ=UTC

RUST_REMAP="--remap-path-prefix=$REPO_ROOT=/build/source"
if [[ -n "${CARGO_HOME:-}" ]]; then
    RUST_REMAP+=" --remap-path-prefix=$CARGO_HOME=/build/cargo"
fi
if [[ -n "${RUSTUP_HOME:-}" ]]; then
    RUST_REMAP+=" --remap-path-prefix=$RUSTUP_HOME=/build/rustup"
fi
if [[ -n "${HOME:-}" ]]; then
    RUST_REMAP+=" --remap-path-prefix=$HOME=/build/home"
fi
export RUSTFLAGS="$RUST_REMAP${RUSTFLAGS:+ $RUSTFLAGS}"

BUILD_ARGS=(build --workspace --release --locked)
if [[ -n "$TARGET_DIR" ]]; then
    if [[ "$TARGET_DIR" != /* ]]; then
        TARGET_DIR="$PWD/$TARGET_DIR"
    fi
    mkdir -p "$TARGET_DIR"
    TARGET_DIR="$(CDPATH= cd -- "$TARGET_DIR" && pwd -P)"
    export CARGO_TARGET_DIR="$TARGET_DIR"
fi

release_note "building ArchiveFS v$VERSION with cargo --release --locked"
(cd "$REPO_ROOT" && cargo "${BUILD_ARGS[@]}")
BUILD_ROOT="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/release"

for binary in archivefs-cli archivefs-gui; do
    [[ -f "$BUILD_ROOT/$binary" && -x "$BUILD_ROOT/$binary" ]] ||
        release_die "release binary missing or not executable: $BUILD_ROOT/$binary"
    install -m 0755 "$BUILD_ROOT/$binary" "$STAGE_ROOT/$binary"
done
install -m 0755 "$REPO_ROOT/install.sh" "$STAGE_ROOT/install.sh"
for document in README.md CHANGELOG.md LICENSE config.toml.example; do
    [[ -f "$REPO_ROOT/$document" ]] || release_die "required release file missing: $document"
    install -m 0644 "$REPO_ROOT/$document" "$STAGE_ROOT/$document"
done
chmod 0755 "$STAGE_ROOT"

release_note "creating deterministic archive $ARTIFACT"
tar \
    --sort=name \
    --format=gnu \
    --mtime="@$SOURCE_DATE_EPOCH" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    -C "$TEMP_ROOT" \
    -cf - "$BUNDLE_NAME" |
    gzip -n -9 >"$ARTIFACT"

ARTIFACT_HASH="$(release_sha256 "$ARTIFACT")"
printf '%s  %s\n' "$ARTIFACT_HASH" "$(basename -- "$ARTIFACT")" >"$CHECKSUM"
chmod 0644 "$ARTIFACT" "$CHECKSUM"

release_note "inspecting and testing the finished archive"
"$SCRIPT_DIR/verify-release-artifact.sh" --checksum "$CHECKSUM" "$ARTIFACT"

printf '\nArchiveFS release artifact created successfully.\n'
printf 'Artifact: %s\n' "$ARTIFACT"
printf 'Checksum: %s\n' "$CHECKSUM"
