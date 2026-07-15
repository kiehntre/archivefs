#!/usr/bin/env bash

set -Eeuo pipefail

PROJECT_DIR="${PROJECT_DIR:-/home/davedap/archivefs}"
NOBARA_HOST="${NOBARA_HOST:-}"
REMOTE_DOWNLOADS="${REMOTE_DOWNLOADS:-/home/davedap/Downloads}"
BUNDLE_NAME="${BUNDLE_NAME:-archivefs-current-test-x86_64-linux}"
EXPECTED_COMMAND_PATTERN="${EXPECTED_COMMAND_PATTERN:-}"
SKIP_TESTS="${SKIP_TESTS:-0}"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

log() {
    printf '\n==> %s\n' "$*"
}

cleanup_local() {
    rm -rf \
        "$PROJECT_DIR/$BUNDLE_NAME" \
        "$PROJECT_DIR/${BUNDLE_NAME}.tar.gz"
}

trap 'printf "\nFailed near line %s\n" "$LINENO" >&2' ERR

[[ -n "$NOBARA_HOST" ]] || die \
    "Set NOBARA_HOST, for example: NOBARA_HOST=davedap@192.168.1.50 $0"

[[ -d "$PROJECT_DIR/.git" ]] || die \
    "ArchiveFS repository not found at $PROJECT_DIR"

cd "$PROJECT_DIR"

log "Checking SSH connection to Nobara"
ssh \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    "$NOBARA_HOST" \
    'printf "Connected to %s\n" "$(hostname)"'

log "Checking working tree"
git status --short
git diff --check

if [[ "$SKIP_TESTS" != "1" ]]; then
    log "Checking formatting"
    cargo fmt --all -- --check

    log "Running Clippy"
    cargo clippy \
        --workspace \
        --all-targets \
        --all-features \
        -- \
        -D warnings

    log "Running workspace tests"
    cargo test --workspace
else
    log "Skipping formatting, Clippy and tests because SKIP_TESTS=1"
fi

log "Building release binaries"
cargo build --workspace --release --locked

CLI="$PROJECT_DIR/target/release/archivefs-cli"
GUI="$PROJECT_DIR/target/release/archivefs-gui"

[[ -x "$CLI" ]] || die "Missing release CLI: $CLI"
[[ -x "$GUI" ]] || die "Missing release GUI: $GUI"

log "Built version"
"$CLI" --version

cleanup_local
mkdir -p "$BUNDLE_NAME"

log "Assembling temporary test bundle"
install -m755 "$CLI" "$BUNDLE_NAME/archivefs-cli"
install -m755 "$GUI" "$BUNDLE_NAME/archivefs-gui"
install -m755 install.sh "$BUNDLE_NAME/install.sh"

for file in README.md CHANGELOG.md; do
    [[ -f "$file" ]] && cp "$file" "$BUNDLE_NAME/"
done

shopt -s nullglob

for file in LICENSE LICENSE-* LICENSES; do
    [[ -e "$file" ]] && cp -r "$file" "$BUNDLE_NAME/"
done

for file in \
    config.toml.example \
    config.example.toml \
    examples/config.toml \
    examples/config.toml.example
do
    [[ -e "$file" ]] && cp "$file" "$BUNDLE_NAME/"
done

shopt -u nullglob

log "Verifying copied binaries"
cmp --silent "$CLI" "$BUNDLE_NAME/archivefs-cli" ||
    die "Bundled CLI differs from target/release/archivefs-cli"

cmp --silent "$GUI" "$BUNDLE_NAME/archivefs-gui" ||
    die "Bundled GUI differs from target/release/archivefs-gui"

"$BUNDLE_NAME/archivefs-cli" --version

if [[ -n "$EXPECTED_COMMAND_PATTERN" ]]; then
    log "Checking expected CLI commands"
    if ! "$BUNDLE_NAME/archivefs-cli" --help |
        grep -E "$EXPECTED_COMMAND_PATTERN"
    then
        die "Expected CLI command pattern not found: $EXPECTED_COMMAND_PATTERN"
    fi
fi

log "Creating tarball"
tar -czf "${BUNDLE_NAME}.tar.gz" "$BUNDLE_NAME"

log "Testing tarball contents"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"; cleanup_local' EXIT

tar -xzf "${BUNDLE_NAME}.tar.gz" -C "$TEST_DIR"

"$TEST_DIR/$BUNDLE_NAME/archivefs-cli" --version

if [[ -n "$EXPECTED_COMMAND_PATTERN" ]]; then
    "$TEST_DIR/$BUNDLE_NAME/archivefs-cli" --help |
        grep -E "$EXPECTED_COMMAND_PATTERN" >/dev/null ||
        die "Expected commands missing from extracted tarball"
fi

log "Copying bundle to Nobara"
scp \
    "${BUNDLE_NAME}.tar.gz" \
    "$NOBARA_HOST:$REMOTE_DOWNLOADS/"

log "Installing and testing on Nobara"
printf -v REMOTE_PATTERN_Q '%q' "$EXPECTED_COMMAND_PATTERN"

ssh "$NOBARA_HOST" \
    "REMOTE_DOWNLOADS=$(printf '%q' "$REMOTE_DOWNLOADS") \
     BUNDLE_NAME=$(printf '%q' "$BUNDLE_NAME") \
     EXPECTED_COMMAND_PATTERN=$(printf '%q' "$EXPECTED_COMMAND_PATTERN") \
     bash -s" <<'REMOTE_SCRIPT'
set -Eeuo pipefail

: "${REMOTE_DOWNLOADS:?REMOTE_DOWNLOADS is required}"
: "${BUNDLE_NAME:?BUNDLE_NAME is required}"
EXPECTED_COMMAND_PATTERN="${EXPECTED_COMMAND_PATTERN:-}"

cd "$REMOTE_DOWNLOADS"

rm -rf "$BUNDLE_NAME"
tar -xzf "${BUNDLE_NAME}.tar.gz"

cd "$BUNDLE_NAME"

printf '\n==> Bundle version before installation\n'
./archivefs-cli --version

if [[ -n "$EXPECTED_COMMAND_PATTERN" ]]; then
    printf '\n==> Checking expected commands before installation\n'
    ./archivefs-cli --help |
        grep -E "$EXPECTED_COMMAND_PATTERN"
fi

DB="$HOME/.local/share/archivefs/library.sqlite3"

if [[ -f "$DB" ]]; then
    BACKUP="${DB}.before-test-$(date +%Y%m%d-%H%M%S)"
    printf '\n==> Backing up database to %s\n' "$BACKUP"
    cp --preserve=mode,timestamps "$DB" "$BACKUP"
else
    printf '\n==> No existing ArchiveFS database to back up\n'
fi

printf '\n==> Installing test build\n'
chmod +x install.sh
./install.sh
hash -r

printf '\n==> Installed binary paths\n'
command -v archivefs-cli
command -v archivefs-gui

printf '\n==> Installed version\n'
archivefs-cli --version

if [[ -n "$EXPECTED_COMMAND_PATTERN" ]]; then
    printf '\n==> Checking expected installed commands\n'
    archivefs-cli --help |
        grep -E "$EXPECTED_COMMAND_PATTERN"
fi

printf '\n==> Running doctor\n'
archivefs-cli doctor

printf '\n==> Checking configuration\n'
archivefs-cli config-check

printf '\n==> Checking persistent library\n'
archivefs-cli library-status

printf '\n==> CLI deployment smoke test passed\n'
REMOTE_SCRIPT

log "Deployment completed successfully"

printf '\nManual GUI test still required on Nobara:\n'
printf '  archivefs-gui\n'
printf '\nTest the new controls, asynchronous refresh, selection behaviour,\n'
printf 'unknown filtering, and confirm mount state does not change.\n'

printf '\nThe Nobara database was backed up before installation.\n'
