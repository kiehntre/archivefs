#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/release-common.sh
source "$SCRIPT_DIR/release-common.sh"

REPO_ROOT="$(release_repo_root)"
release_require_command git
release_require_command python3

git -C "$REPO_ROOT" ls-files -z |
    python3 -c '
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
paths = [path for path in sys.stdin.buffer.read().split(b"\0") if path]
patterns = [
    ("private key", re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
    ("GitHub token", re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})")),
    ("AWS access key", re.compile(rb"AKIA[0-9A-Z]{16}")),
    ("credential assignment", re.compile(rb"(?:GITHUB_TOKEN|GH_TOKEN|AWS_SECRET_ACCESS_KEY|CARGO_REGISTRY_TOKEN)=[^\s]+")),
    ("credential-bearing URL", re.compile(rb"https?://[^\s/:]+:[^\s/@]+@")),
]
findings = []
for raw_path in paths:
    relative = pathlib.Path(raw_path.decode("utf-8", "surrogateescape"))
    path = root / relative
    try:
        data = path.read_bytes()
    except OSError as error:
        findings.append(f"could not read tracked file {relative}: {error}")
        continue
    for label, pattern in patterns:
        match = pattern.search(data)
        if match:
            excerpt = match.group(0)[:120].decode("utf-8", "backslashreplace")
            findings.append(f"{relative}: {label}: {excerpt}")
if findings:
    print("\n".join(findings), file=sys.stderr)
    raise SystemExit(1)
print(f"release: scanned {len(paths)} tracked files; no credential-shaped secrets found")
' "$REPO_ROOT"
