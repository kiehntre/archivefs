#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

[[ $# -eq 1 ]] || release_die "usage: scripts/test-release-artifact-verifier.sh ARCHIVE.tar.gz"
VALID_ARTIFACT=$1
[[ -f "$VALID_ARTIFACT" ]] || release_die "archive not found: $VALID_ARTIFACT"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/archivefs-verifier-tests.XXXXXXXX")"
cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

ARCHIVE_NAME="$(basename -- "$VALID_ARTIFACT")"

write_checksum() {
    local archive=$1
    (cd "$(dirname -- "$archive")" && sha256sum "$(basename -- "$archive")") >"$archive.sha256"
}

expect_failure() {
    local label=$1
    local archive=$2
    if "$SCRIPT_DIR/verify-release-artifact.sh" "$archive" >"$TEMP_ROOT/$label.out" 2>&1; then
        release_die "verifier accepted malformed fixture: $label"
    fi
    release_note "malformed artifact rejected: $label"
}

mkdir -p "$TEMP_ROOT/bad-checksum"
cp "$VALID_ARTIFACT" "$TEMP_ROOT/bad-checksum/$ARCHIVE_NAME"
printf '%064d  %s\n' 0 "$ARCHIVE_NAME" >"$TEMP_ROOT/bad-checksum/$ARCHIVE_NAME.sha256"
expect_failure bad-checksum "$TEMP_ROOT/bad-checksum/$ARCHIVE_NAME"

python3 - "$VALID_ARTIFACT" "$TEMP_ROOT" "$ARCHIVE_NAME" <<'PY'
import copy
import io
import pathlib
import sys
import tarfile

source_path = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
archive_name = sys.argv[3]

with tarfile.open(source_path, "r:gz") as source:
    originals = []
    for member in source.getmembers():
        data = source.extractfile(member).read() if member.isfile() else None
        originals.append((copy.copy(member), data))

def write_case(name, mutation):
    directory = root / name
    directory.mkdir()
    output = directory / archive_name
    members = [(copy.copy(member), data) for member, data in originals]
    mutation(members)
    with tarfile.open(output, "w:gz") as target:
        for member, data in members:
            target.addfile(member, io.BytesIO(data) if data is not None else None)

def unexpected(members):
    root_name = members[0][0].name.split("/", 1)[0]
    info = tarfile.TarInfo(f"{root_name}/unexpected.txt")
    info.mode = 0o644
    info.uid = info.gid = 0
    info.size = 10
    members.append((info, b"unexpected"))

def traversal(members):
    info = tarfile.TarInfo("../escape")
    info.mode = 0o644
    info.uid = info.gid = 0
    info.size = 6
    members.append((info, b"escape"))

def bad_mode(members):
    for member, _ in members:
        if member.name.endswith("/archivefs-cli"):
            member.mode = 0o777
            return
    raise RuntimeError("CLI member missing")

def privacy_leak(members):
    for position, (member, data) in enumerate(members):
        if member.name.endswith("/README.md"):
            data += b"\n/home/davedap/private/build\n"
            member.size = len(data)
            members[position] = (member, data)
            return
    raise RuntimeError("README member missing")

write_case("unexpected", unexpected)
write_case("traversal", traversal)
write_case("bad-mode", bad_mode)
write_case("privacy-leak", privacy_leak)
PY

for label in unexpected traversal bad-mode privacy-leak; do
    archive="$TEMP_ROOT/$label/$ARCHIVE_NAME"
    write_checksum "$archive"
    expect_failure "$label" "$archive"
done
[[ ! -e "$TEMP_ROOT/escape" ]] || release_die "path traversal fixture escaped extraction"

release_note "artifact verifier negative tests passed"
