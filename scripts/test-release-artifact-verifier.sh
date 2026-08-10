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
REPO_ROOT="$(release_repo_root)"
VERSION="$(release_workspace_version "$REPO_ROOT")"
BUNDLE_NAME="${ARCHIVE_NAME%.tar.gz}"

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

python3 - "$TEMP_ROOT" "$ARCHIVE_NAME" "$BUNDLE_NAME" "$VERSION" <<'PY'
import io
import pathlib
import sys
import tarfile

output_root = pathlib.Path(sys.argv[1])
archive_name, bundle_name, version = sys.argv[2:]

def base_members():
    cli = f"#!/bin/sh\nprintf '%s\\n' 'emuwiz-cli {version}'\n".encode()
    gui = f"#!/bin/sh\nprintf '%s\\n' 'emuwiz {version}'\n".encode()
    values = {
        "emuwiz-cli": (0o755, cli),
        "emuwiz": (0o755, gui),
        "install.sh": (0o755, b"#!/bin/sh\nexit 0\n"),
        "README.md": (0o644, b"EmuWiz release fixture\n"),
        "CHANGELOG.md": (0o644, b"Release fixture\n"),
        "LICENSE": (0o644, b"MIT fixture\n"),
        "config.toml.example": (0o644, b"mount_dir = '/tmp/archivefs'\n"),
    }
    root_info = tarfile.TarInfo(bundle_name)
    root_info.type = tarfile.DIRTYPE
    root_info.mode = 0o755
    root_info.uid = root_info.gid = 0
    members = [(root_info, None)]
    for name, (mode, data) in values.items():
        info = tarfile.TarInfo(f"{bundle_name}/{name}")
        info.mode = mode
        info.uid = info.gid = 0
        info.size = len(data)
        members.append((info, data))
    return members

def write_case(name, mutation):
    directory = output_root / name
    directory.mkdir()
    output = directory / archive_name
    members = base_members()
    mutation(members)
    with tarfile.open(output, "w:gz") as target:
        for member, data in members:
            target.addfile(member, io.BytesIO(data) if data is not None else None)

def unexpected(members):
    info = tarfile.TarInfo(f"{bundle_name}/unexpected.txt")
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
        if member.name.endswith("/emuwiz-cli"):
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
