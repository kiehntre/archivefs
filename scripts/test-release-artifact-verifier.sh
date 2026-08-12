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

python3 - "$TEMP_ROOT" "$ARCHIVE_NAME" "$BUNDLE_NAME" "$VERSION" "$REPO_ROOT" <<'PY'
import binascii
import io
import pathlib
import struct
import sys
import tarfile
import zlib

output_root = pathlib.Path(sys.argv[1])
archive_name, bundle_name, version = sys.argv[2:5]
repo_root = pathlib.Path(sys.argv[5])

def base_members():
    cli = f"#!/bin/sh\nprintf '%s\\n' 'emuwiz-cli {version}'\n".encode()
    gui = f"#!/bin/sh\nprintf '%s\\n' 'emuwiz {version}'\n".encode()
    values = {
        "emuwiz-cli": (0o755, cli),
        "emuwiz": (0o755, gui),
        "install.sh": (0o755, (repo_root / "install.sh").read_bytes()),
        "README.md": (0o644, b"EmuWiz release fixture\n"),
        "CHANGELOG.md": (0o644, b"Release fixture\n"),
        "LICENSE": (0o644, b"MIT fixture\n"),
        "config.toml.example": (0o644, b"mount_dir = '/tmp/archivefs'\n"),
        "assets/linux/io.github.kiehntre.emuwiz.desktop.in": (
            0o644,
            (repo_root / "assets/linux/io.github.kiehntre.emuwiz.desktop.in").read_bytes(),
        ),
    }
    for size in (32, 64, 128, 256, 512):
        relative = f"assets/branding/emuwiz-logo-{size}.png"
        values[relative] = (0o644, (repo_root / relative).read_bytes())
    root_info = tarfile.TarInfo(bundle_name)
    root_info.type = tarfile.DIRTYPE
    root_info.mode = 0o755
    root_info.uid = root_info.gid = 0
    members = [(root_info, None)]
    for directory in ("assets", "assets/branding", "assets/linux"):
        info = tarfile.TarInfo(f"{bundle_name}/{directory}")
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
        info.uid = info.gid = 0
        members.append((info, None))
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

def missing_icon(members):
    members[:] = [
        item for item in members
        if not item[0].name.endswith("/assets/branding/emuwiz-logo-64.png")
    ]

def png_chunks(data):
    offset = 8
    chunks = []
    while offset < len(data):
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        kind = data[offset + 4 : offset + 8]
        end = offset + 12 + length
        chunks.append((kind, data[offset + 8 : offset + 8 + length]))
        offset = end
    return chunks

def png_chunk_bytes(kind, payload):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", binascii.crc32(kind + payload) & 0xFFFFFFFF)
    )

def repack_png_with_one_pixel_changed(data):
    # Produces a *structurally valid* PNG - correct signature, CRCs, IHDR,
    # and decodable IDAT - that differs from the approved source by exactly
    # one decoded pixel byte. This is what actually exercises the
    # verifier's "differs from approved source" identity check; a bit-flip
    # on the raw file bytes (the previous approach) usually just corrupts
    # the IEND CRC and gets rejected by PNG structural validation instead,
    # never reaching the identity check it's meant to test.
    chunks = png_chunks(data)
    compressed = b"".join(payload for kind, payload in chunks if kind == b"IDAT")
    pixels = bytearray(zlib.decompress(compressed))
    pixels[-1] ^= 0x01  # last byte of the last row: never a filter byte
    new_idat = png_chunk_bytes(b"IDAT", zlib.compress(bytes(pixels), 9))
    rebuilt = bytearray(b"\x89PNG\r\n\x1a\n")
    replaced_idat = False
    for kind, payload in chunks:
        if kind == b"IDAT":
            if not replaced_idat:
                rebuilt += new_idat
                replaced_idat = True
            continue
        rebuilt += png_chunk_bytes(kind, payload)
    return bytes(rebuilt)

def substituted_icon(members):
    for position, (member, data) in enumerate(members):
        if member.name.endswith("/assets/branding/emuwiz-logo-128.png"):
            replacement = repack_png_with_one_pixel_changed(data)
            member.size = len(replacement)
            members[position] = (member, replacement)
            return
    raise RuntimeError("128px icon member missing")

def malformed_png(members):
    for position, (member, data) in enumerate(members):
        if member.name.endswith("/assets/branding/emuwiz-logo-256.png"):
            replacement = b"not a png"
            member.size = len(replacement)
            members[position] = (member, replacement)
            return
    raise RuntimeError("256px icon member missing")

def duplicate_member(members):
    for member, data in members:
        if member.name.endswith("/emuwiz-cli"):
            duplicate_info = tarfile.TarInfo(member.name)
            duplicate_info.mode = member.mode
            duplicate_info.uid = duplicate_info.gid = 0
            duplicate_info.size = member.size
            members.append((duplicate_info, data))
            return
    raise RuntimeError("emuwiz-cli member missing")

def malformed_desktop(members):
    for position, (member, data) in enumerate(members):
        if member.name.endswith("/assets/linux/io.github.kiehntre.emuwiz.desktop.in"):
            replacement = data.replace(b"Type=Application", b"Type=Broken")
            member.size = len(replacement)
            members[position] = (member, replacement)
            return
    raise RuntimeError("desktop member missing")

def tampered_installer(members):
    for position, (member, data) in enumerate(members):
        if member.name.endswith("/install.sh"):
            replacement = data + b"\n# a single appended byte is enough to fail byte-identity\n"
            member.size = len(replacement)
            members[position] = (member, replacement)
            return
    raise RuntimeError("install.sh member missing")

write_case("unexpected", unexpected)
write_case("traversal", traversal)
write_case("bad-mode", bad_mode)
write_case("privacy-leak", privacy_leak)
write_case("missing-icon", missing_icon)
write_case("substituted-icon", substituted_icon)
write_case("malformed-png", malformed_png)
write_case("malformed-desktop", malformed_desktop)
write_case("tampered-installer", tampered_installer)
write_case("duplicate-member", duplicate_member)
PY

for label in unexpected traversal bad-mode privacy-leak missing-icon substituted-icon malformed-png malformed-desktop tampered-installer duplicate-member; do
    archive="$TEMP_ROOT/$label/$ARCHIVE_NAME"
    write_checksum "$archive"
    expect_failure "$label" "$archive"
done
[[ ! -e "$TEMP_ROOT/escape" ]] || release_die "path traversal fixture escaped extraction"

# tampered-installer specifically must fail for install.sh byte-identity -
# every other fixture above tampers with something that fails earlier
# structural checks first, so a generic "the verifier rejected it" alone
# wouldn't distinguish this from a fixture that happened to be caught for
# an unrelated reason.
grep -q 'release install.sh differs from the canonical script in the repository' \
    "$TEMP_ROOT/tampered-installer.out" ||
    release_die "tampered-installer fixture was rejected for the wrong reason (expected the install.sh byte-identity check to fire): $(cat "$TEMP_ROOT/tampered-installer.out")"
release_note "tampered-installer fixture rejected specifically for install.sh byte-identity"

release_note "artifact verifier negative tests passed"
