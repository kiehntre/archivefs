#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

usage() {
    cat <<'EOF'
Usage: scripts/verify-release-artifact.sh [--checksum FILE] ARCHIVE.tar.gz

Verify an existing canonical EmuWiz release archive without launching the GUI.
The checksum defaults to ARCHIVE.tar.gz.sha256.
EOF
}

CHECKSUM=""
while (($#)); do
    case "$1" in
        --checksum)
            (($# >= 2)) || release_die "--checksum requires a file"
            CHECKSUM=$2
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --*) release_die "unknown argument: $1" ;;
        *)
            [[ -z "${ARTIFACT:-}" ]] || release_die "accepts exactly one release archive"
            ARTIFACT=$1
            shift
            ;;
    esac
done

[[ -n "${ARTIFACT:-}" ]] || release_die "release archive is required"
for command in cargo python3 sha256sum strings; do
    release_require_command "$command"
done

REPO_ROOT="$(release_repo_root)"
VERSION="$(release_workspace_version "$REPO_ROOT")"
TARGET_NAME="$(release_target_name)"
EXPECTED_ROOT="$(release_bundle_name "$VERSION" "$TARGET_NAME")"

ARTIFACT="$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$ARTIFACT")"
[[ -f "$ARTIFACT" ]] || release_die "archive not found: $ARTIFACT"
CHECKSUM="${CHECKSUM:-$ARTIFACT.sha256}"
CHECKSUM="$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$CHECKSUM")"
[[ -f "$CHECKSUM" ]] || release_die "checksum file not found: $CHECKSUM"

EXPECTED_ARCHIVE_NAME="$EXPECTED_ROOT.tar.gz"
[[ "$(basename -- "$ARTIFACT")" == "$EXPECTED_ARCHIVE_NAME" ]] ||
    release_die "artifact name must be $EXPECTED_ARCHIVE_NAME"
[[ "$(basename -- "$CHECKSUM")" == "$EXPECTED_ARCHIVE_NAME.sha256" ]] ||
    release_die "checksum name must be $EXPECTED_ARCHIVE_NAME.sha256"

read -r EXPECTED_HASH CHECKSUM_NAME EXTRA <"$CHECKSUM" || release_die "could not read checksum"
[[ -z "${EXTRA:-}" ]] || release_die "checksum file must contain exactly one hash record"
CHECKSUM_NAME="${CHECKSUM_NAME#\*}"
[[ "$EXPECTED_HASH" =~ ^[0-9a-f]{64}$ ]] || release_die "checksum is not SHA-256"
[[ "$CHECKSUM_NAME" == "$EXPECTED_ARCHIVE_NAME" ]] ||
    release_die "checksum names '$CHECKSUM_NAME', expected '$EXPECTED_ARCHIVE_NAME'"
ACTUAL_HASH="$(release_sha256 "$ARTIFACT")"
[[ "$ACTUAL_HASH" == "$EXPECTED_HASH" ]] || release_die "archive SHA-256 does not match"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/archivefs-verify.XXXXXXXX")"
cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM
EXTRACT_ROOT="$TEMP_ROOT/extracted"
mkdir -p "$EXTRACT_ROOT"

# Validate every member before extraction. Only regular files and the one
# root directory are accepted, making symlink/hardlink and path traversal
# attacks impossible even if a future Python extraction implementation changes.
python3 - "$ARTIFACT" "$EXTRACT_ROOT" "$EXPECTED_ROOT" <<'PY'
import os
import pathlib
import sys
import tarfile

archive, destination, root = sys.argv[1:]
expected_modes = {
    f"{root}/": 0o755,
    f"{root}/emuwiz-cli": 0o755,
    f"{root}/emuwiz": 0o755,
    f"{root}/install.sh": 0o755,
    f"{root}/README.md": 0o644,
    f"{root}/CHANGELOG.md": 0o644,
    f"{root}/LICENSE": 0o644,
    f"{root}/config.toml.example": 0o644,
    f"{root}/assets/": 0o755,
    f"{root}/assets/branding/": 0o755,
    f"{root}/assets/linux/": 0o755,
    f"{root}/assets/linux/io.github.kiehntre.emuwiz.desktop.in": 0o644,
    f"{root}/assets/branding/emuwiz-logo-32.png": 0o644,
    f"{root}/assets/branding/emuwiz-logo-64.png": 0o644,
    f"{root}/assets/branding/emuwiz-logo-128.png": 0o644,
    f"{root}/assets/branding/emuwiz-logo-256.png": 0o644,
    f"{root}/assets/branding/emuwiz-logo-512.png": 0o644,
}

with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    names = set()
    for member in members:
        name = member.name + ("/" if member.isdir() and not member.name.endswith("/") else "")
        pure = pathlib.PurePosixPath(member.name)
        if pure.is_absolute() or ".." in pure.parts or not pure.parts or pure.parts[0] != root:
            raise SystemExit(f"unsafe archive member path: {member.name!r}")
        if name in names:
            raise SystemExit(f"duplicate archive member: {name}")
        names.add(name)
        if name not in expected_modes:
            raise SystemExit(f"unexpected archive member: {name}")
        if name.endswith("/"):
            if not member.isdir():
                raise SystemExit(f"release directory member is not a directory: {name}")
        elif not member.isfile():
            raise SystemExit(f"release member is not a regular file: {name}")
        if member.uid != 0 or member.gid != 0:
            raise SystemExit(f"non-neutral ownership for {name}: {member.uid}:{member.gid}")
        actual_mode = member.mode & 0o7777
        if actual_mode != expected_modes[name]:
            raise SystemExit(
                f"unsafe mode for {name}: {actual_mode:#06o}, expected {expected_modes[name]:#06o}"
            )
    missing = set(expected_modes) - names
    if missing:
        raise SystemExit(f"missing release members: {sorted(missing)}")

    destination_real = os.path.realpath(destination)
    for member in members:
        output = os.path.realpath(os.path.join(destination, member.name))
        if os.path.commonpath([destination_real, output]) != destination_real:
            raise SystemExit(f"archive extraction would escape destination: {member.name!r}")
    bundle.extractall(destination, members=members, filter="data")
PY

PAYLOAD="$EXTRACT_ROOT/$EXPECTED_ROOT"
[[ -d "$PAYLOAD" ]] || release_die "expected extracted root missing: $EXPECTED_ROOT"

# The installer payload must contain the exact approved source assets, not
# lookalikes with the same names. Validate the PNG structure first, and
# independently of the byte-identity check below, so a structurally invalid
# or corrupted PNG is always rejected by the format parser itself rather
# than only by the (necessarily earlier-short-circuiting) identity check -
# otherwise the parser below would never actually be exercised.
if command -v desktop-file-validate >/dev/null 2>&1; then
    DESKTOP_VALIDATION_COPY="$TEMP_ROOT/io.github.kiehntre.emuwiz.desktop"
    cp -- "$PAYLOAD/assets/linux/io.github.kiehntre.emuwiz.desktop.in" \
        "$DESKTOP_VALIDATION_COPY"
    desktop-file-validate "$DESKTOP_VALIDATION_COPY"
else
    release_note "desktop-file-validate unavailable; canonical desktop template comparison still passed"
fi

python3 - "$REPO_ROOT" "$PAYLOAD" <<'PY'
import binascii
import pathlib
import struct
import sys
import zlib

repo, payload = map(pathlib.Path, sys.argv[1:])
desktop_rel = pathlib.Path("assets/linux/io.github.kiehntre.emuwiz.desktop.in")
if (payload / desktop_rel).read_bytes() != (repo / desktop_rel).read_bytes():
    raise SystemExit("release desktop entry differs from the canonical template")

for size in (32, 64, 128, 256, 512):
    relative = pathlib.Path(f"assets/branding/emuwiz-logo-{size}.png")
    data = (payload / relative).read_bytes()
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        raise SystemExit(f"invalid PNG signature: {relative}")
    offset = 8
    ihdr = None
    compressed = bytearray()
    saw_iend = False
    while offset < len(data):
        if offset + 12 > len(data):
            raise SystemExit(f"truncated PNG chunk: {relative}")
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        kind = data[offset + 4 : offset + 8]
        end = offset + 12 + length
        if end > len(data):
            raise SystemExit(f"truncated PNG payload: {relative}")
        chunk = data[offset + 8 : offset + 8 + length]
        expected_crc = struct.unpack(">I", data[offset + 8 + length : end])[0]
        actual_crc = binascii.crc32(kind + chunk) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise SystemExit(f"PNG CRC mismatch: {relative}")
        if kind == b"IHDR":
            ihdr = chunk
        elif kind == b"IDAT":
            compressed.extend(chunk)
        elif kind == b"IEND":
            saw_iend = True
            if end != len(data):
                raise SystemExit(f"trailing data after PNG IEND: {relative}")
        offset = end
    if ihdr is None or len(ihdr) != 13 or not saw_iend:
        raise SystemExit(f"incomplete PNG structure: {relative}")
    width, height, depth, colour, compression, filtering, interlace = struct.unpack(
        ">IIBBBBB", ihdr
    )
    if (width, height) != (size, size):
        raise SystemExit(f"wrong PNG dimensions for {relative}: {width}x{height}")
    if (depth, colour, compression, filtering, interlace) != (8, 6, 0, 0, 0):
        raise SystemExit(f"icon is not non-interlaced 8-bit RGBA: {relative}")
    try:
        pixels = zlib.decompress(compressed)
    except zlib.error as error:
        raise SystemExit(f"invalid PNG image data for {relative}: {error}") from error
    row_bytes = width * 4 + 1
    if len(pixels) != row_bytes * height:
        raise SystemExit(f"wrong decoded PNG data length: {relative}")
    if any(pixels[row * row_bytes] > 4 for row in range(height)):
        raise SystemExit(f"invalid PNG row filter: {relative}")
    # Structural validity established above; now confirm it's the exact
    # approved asset and not merely a well-formed lookalike.
    if data != (repo / relative).read_bytes():
        raise SystemExit(f"release icon differs from approved source: {relative}")
PY

EXPECTED_CLI="emuwiz-cli $VERSION"
EXPECTED_GUI="emuwiz $VERSION"
CLI_VERSION="$(env -i PATH="${PATH:-/usr/bin:/bin}" HOME="$TEMP_ROOT/home" "$PAYLOAD/emuwiz-cli" --version)"
GUI_VERSION="$(env -i PATH="${PATH:-/usr/bin:/bin}" HOME="$TEMP_ROOT/home" "$PAYLOAD/emuwiz" --version)"
[[ "$CLI_VERSION" == "$EXPECTED_CLI" ]] ||
    release_die "CLI version mismatch: '$CLI_VERSION', expected '$EXPECTED_CLI'"
[[ "$GUI_VERSION" == "$EXPECTED_GUI" ]] ||
    release_die "GUI version mismatch: '$GUI_VERSION', expected '$EXPECTED_GUI'"

# Scan text plus printable binary strings. The patterns intentionally target
# credential shapes and machine-specific absolute paths, not ordinary words
# such as "token" that can legitimately occur in documentation or binaries.
python3 - "$PAYLOAD" <<'PY'
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
patterns = [
    ("maintainer path", re.compile(rb"/home/davedap(?:/|\x00|\s)")),
    ("private key", re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
    ("GitHub token", re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})")),
    ("AWS access key", re.compile(rb"AKIA[0-9A-Z]{16}")),
    ("credential assignment", re.compile(rb"(?:GITHUB_TOKEN|GH_TOKEN|AWS_SECRET_ACCESS_KEY|CARGO_REGISTRY_TOKEN)=[^\s\x00]+")),
    ("credential-bearing URL", re.compile(rb"https?://[^\s/:]+:[^\s/@]+@")),
    (
        "absolute build path",
        re.compile(
            rb"(?<!/build)(?:/(?:home|Users)/[A-Za-z0-9_-]+/|/(?:runner/work|workspace|builds)/)"
            rb"(?:[^\s\x00]+)"
        ),
    ),
]
allowed_prefixes = (b"/build/source", b"/build/home", b"/build/cargo", b"/build/rustup")

for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
    if path.name in {"emuwiz-cli", "emuwiz"}:
        data = subprocess.run(
            ["strings", "-a", str(path)], check=True, stdout=subprocess.PIPE
        ).stdout
    else:
        data = path.read_bytes()
    for label, pattern in patterns:
        for match in pattern.finditer(data):
            value = match.group(0)
            if label == "absolute build path" and value.startswith(allowed_prefixes):
                continue
            display = value[:160].decode("utf-8", "backslashreplace")
            raise SystemExit(f"{label} found in {path.name}: {display}")
PY

release_note "artifact structure, ownership, modes, checksum, privacy, and versions verified"
release_note "CLI: $CLI_VERSION"
release_note "GUI: $GUI_VERSION (version-only path; GUI window was not launched)"
release_note "verified artifact: $ARTIFACT"
