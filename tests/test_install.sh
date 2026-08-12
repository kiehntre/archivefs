#!/bin/sh
# Shell-level tests for install.sh.
#
# Exercises install.sh entirely inside temporary directories with HOME
# overridden to a scratch path - it never reads or writes the real
# $HOME. Run it directly:
#   sh tests/test_install.sh
# Exits 0 if every test passes, 1 if any test fails.
set -eu

script_dir=$(dirname -- "$0")
script_dir=$(CDPATH= cd -- "$script_dir" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
install_sh="$repo_root/install.sh"
config_example="$repo_root/config.toml.example"
branding_assets="$repo_root/assets/branding"
desktop_assets="$repo_root/assets/linux"

[ -f "$install_sh" ] || { printf 'test_install.sh: cannot find %s\n' "$install_sh" >&2; exit 1; }
[ -f "$config_example" ] || { printf 'test_install.sh: cannot find %s\n' "$config_example" >&2; exit 1; }

# fake_ratarmount_dir - a scratch directory on PATH holding a no-op
# `ratarmount` executable, prepended to PATH only for the handful of tests
# below that assert stderr is completely empty. Those tests are about
# ownership/foreign-collision warnings, not about whether ratarmount
# happens to be installed on whatever machine runs this suite - without
# this, they fail in CI (no ratarmount on PATH there) even though nothing
# about ownership behavior is wrong. This does not suppress or mask
# install.sh's real ratarmount warning: it makes ratarmount genuinely
# "found" for that one invocation, exactly like a real install would see
# on a machine that has it, which is exactly the "warns when ratarmount is
# not on PATH" test elsewhere in this file already covers from the other
# direction.
fake_ratarmount_dir=$(mktemp -d)
trap 'rm -rf -- "$fake_ratarmount_dir"' EXIT
printf '#!/bin/sh\nexit 0\n' >"$fake_ratarmount_dir/ratarmount"
chmod +x -- "$fake_ratarmount_dir/ratarmount"

pass_count=0
fail_count=0

ok() {
    pass_count=$((pass_count + 1))
    printf 'ok - %s\n' "$*"
}

bad() {
    fail_count=$((fail_count + 1))
    printf 'NOT OK - %s\n' "$*"
}

# assert_success DESCRIPTION CMD... : runs CMD, records ok/bad. Safe under
# `set -e` because the failing command only ever appears in an `if`
# condition, which POSIX exempts from triggering -e.
assert_success() {
    description="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        ok "$description"
    else
        bad "$description (expected success, command failed)"
    fi
}

# assert_failure DESCRIPTION CMD... : runs CMD, expects a nonzero exit.
assert_failure() {
    description="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        bad "$description (expected failure, command succeeded)"
    else
        ok "$description"
    fi
}

assert_file_exists() {
    if [ -f "$2" ]; then ok "$1"; else bad "$1 (missing: $2)"; fi
}

assert_no_such_path() {
    if [ -e "$2" ] || [ -L "$2" ]; then bad "$1 (still present: $2)"; else ok "$1"; fi
}

assert_executable() {
    if [ -x "$2" ]; then ok "$1"; else bad "$1 (not executable: $2)"; fi
}

assert_files_equal() {
    if cmp -s -- "$2" "$3"; then ok "$1"; else bad "$1 ($2 differs from $3)"; fi
}

assert_contains() {
    case "$2" in
        *"$3"*) ok "$1" ;;
        *) bad "$1 (did not find: $3)" ;;
    esac
}

# decode_desktop_exec EXEC_LINE - decodes an "Exec=..." Desktop Entry line
# and prints the resulting literal string. Mirrors the two-layer escaping a
# real consumer (e.g. desktop-file-utils' handle_exec_key()) applies: first
# the format's generic string-value unescape (only "\\" -> "\" matters
# here), THEN the Exec-specific quoted-argument unescape (\", \`, \$, \\,
# and %% -> %). Used to prove the value install.sh writes round-trips back
# to the exact original path, not just that it looks plausible.
decode_desktop_exec() {
    python3 - "$1" <<'PY'
import sys

line = sys.argv[1]
assert line.startswith('Exec=')
raw = line[len("Exec=") :]

# Layer 1: generic Desktop Entry string-value unescape.
layer1 = []
i = 0
while i < len(raw):
    if raw[i] == "\\" and i + 1 < len(raw) and raw[i + 1] == "\\":
        layer1.append("\\")
        i += 2
        continue
    layer1.append(raw[i])
    i += 1
layer1 = "".join(layer1)

assert layer1.startswith('"') and layer1.endswith('"'), layer1
inner = layer1[1:-1]

# Layer 2: Exec quoted-argument unescape.
out = []
i = 0
while i < len(inner):
    char = inner[i]
    if char == "\\" and i + 1 < len(inner) and inner[i + 1] in '"`$\\':
        out.append(inner[i + 1])
        i += 2
        continue
    if char == "%" and i + 1 < len(inner) and inner[i + 1] == "%":
        out.append("%")
        i += 2
        continue
    out.append(char)
    i += 1
sys.stdout.write("".join(out))
PY
}

# make_bundle DIR - populates DIR with a fake extracted release bundle:
# install.sh, stub emuwiz-cli/emuwiz, config.toml.example.
make_bundle() {
    mkdir -p -- "$1"
    cp -- "$install_sh" "$1/install.sh"
    printf '#!/bin/sh\necho fake-cli\n' >"$1/emuwiz-cli"
    printf '#!/bin/sh\necho fake-gui\n' >"$1/emuwiz"
    chmod +x -- "$1/emuwiz-cli" "$1/emuwiz"
    cp -- "$config_example" "$1/config.toml.example"
    mkdir -p -- "$1/assets"
    cp -R -- "$branding_assets" "$1/assets/branding"
    cp -R -- "$desktop_assets" "$1/assets/linux"
}

# make_workspace DIR - populates DIR with a fake workspace checkout:
# install.sh at the root, stub binaries under target/release/,
# config.toml.example at the root.
make_workspace() {
    mkdir -p -- "$1/target/release"
    cp -- "$install_sh" "$1/install.sh"
    printf '#!/bin/sh\necho ws-cli\n' >"$1/target/release/emuwiz-cli"
    printf '#!/bin/sh\necho ws-gui\n' >"$1/target/release/emuwiz"
    chmod +x -- "$1/target/release/emuwiz-cli" "$1/target/release/emuwiz"
    cp -- "$config_example" "$1/config.toml.example"
    mkdir -p -- "$1/assets"
    cp -R -- "$branding_assets" "$1/assets/branding"
    cp -R -- "$desktop_assets" "$1/assets/linux"
}

echo "=== test: --help exits 0 and prints usage ==="
help_out=$(sh "$install_sh" --help)
assert_success "--help exits 0" sh "$install_sh" --help
assert_contains "--help mentions --uninstall" "$help_out" "--uninstall"

echo "=== test: fresh install from a release bundle ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

assert_success "install from bundle succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
assert_executable "emuwiz-cli installed and executable" "$bin_dir/emuwiz-cli"
assert_executable "archivefs-cli legacy alias installed" "$bin_dir/archivefs-cli"
assert_executable "emuwiz installed and executable" "$bin_dir/emuwiz"
assert_executable "emuwiz-gui alias installed" "$bin_dir/emuwiz-gui"
assert_executable "archivefs-gui legacy alias installed" "$bin_dir/archivefs-gui"
desktop_file="$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
assert_file_exists "desktop entry installed" "$desktop_file"
desktop_content=$(cat "$desktop_file")
assert_contains "desktop entry names the stable icon" "$desktop_content" \
    "Icon=io.github.kiehntre.emuwiz"
assert_contains "desktop entry uses the absolute canonical binary" "$desktop_content" \
    "Exec=\"$bin_dir/emuwiz\""
for size in 32 64 128 256 512; do
    installed_icon="$home/.local/share/icons/hicolor/${size}x${size}/apps/io.github.kiehntre.emuwiz.png"
    assert_files_equal "$size pixel application icon is the approved asset" \
        "$branding_assets/emuwiz-logo-$size.png" "$installed_icon"
done
if command -v desktop-file-validate >/dev/null 2>&1; then
    assert_success "installed desktop entry passes desktop-file-validate" \
        desktop-file-validate "$desktop_file"
else
    printf 'SKIP - desktop-file-validate is unavailable\n'
fi

assert_file_exists "config.toml created" "$home/.config/emuwiz/config.toml"
assert_files_equal "config.toml matches config.toml.example" \
    "$config_example" "$home/.config/emuwiz/config.toml"
cli_out=$("$bin_dir/emuwiz-cli")
assert_contains "installed emuwiz-cli runs (bundle stub)" "$cli_out" "fake-cli"
rm -rf -- "$work"

echo "=== test: custom XDG data home and a prefix containing spaces ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
data_home="$work/custom data"
bin_dir="$work/prefix with spaces/bin"
mkdir -p -- "$home"

assert_success "install with custom XDG data home and spaced prefix succeeds" \
    env HOME="$home" XDG_DATA_HOME="$data_home" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir"
desktop_file="$data_home/applications/io.github.kiehntre.emuwiz.desktop"
desktop_content=$(cat "$desktop_file")
assert_contains "spaced absolute executable is quoted in desktop entry" "$desktop_content" \
    "Exec=\"$bin_dir/emuwiz\""
assert_file_exists "custom XDG data home receives icons" \
    "$data_home/icons/hicolor/512x512/apps/io.github.kiehntre.emuwiz.png"
assert_no_such_path "default data home is not used when XDG_DATA_HOME is set" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"

cp -- "$desktop_file" "$work/desktop.before"
cp -- "$data_home/icons/hicolor/256x256/apps/io.github.kiehntre.emuwiz.png" \
    "$work/icon.before"
assert_success "reinstall is idempotent" \
    env HOME="$home" XDG_DATA_HOME="$data_home" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir"
assert_files_equal "reinstall leaves desktop content stable" "$work/desktop.before" "$desktop_file"
assert_files_equal "reinstall leaves icon content stable" "$work/icon.before" \
    "$data_home/icons/hicolor/256x256/apps/io.github.kiehntre.emuwiz.png"
rm -rf -- "$work"

echo "=== test: Exec value round-trips a prefix with shell-special characters ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
weird_name='weird $dollar `backtick` "quote" back\slash %percent'
bin_dir="$work/$weird_name"

assert_success "install with a shell-special-character prefix succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
desktop_file="$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
exec_line=$(grep '^Exec=' "$desktop_file")
decoded=$(decode_desktop_exec "$exec_line")
if [ "$decoded" = "$bin_dir/emuwiz" ]; then
    ok "Exec value decodes back to the exact original path"
else
    bad "Exec value decodes back to the exact original path (got: $decoded)"
fi
if command -v desktop-file-validate >/dev/null 2>&1; then
    assert_success "desktop entry with special-character Exec passes desktop-file-validate" \
        desktop-file-validate "$desktop_file"
else
    printf 'SKIP - desktop-file-validate is unavailable\n'
fi
rm -rf -- "$work"

echo "=== test: an install prefix containing a line break is rejected ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
newline=$(printf 'a\nb')
bin_dir="$work/pre${newline}post"
mkdir -p -- "$bin_dir"

assert_failure "install rejects a prefix containing a line break" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
assert_no_such_path "no desktop entry was written for the rejected install" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
rm -rf -- "$work"

echo "=== test: a relative XDG_DATA_HOME falls back to ~/.local/share ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

warn=$(env HOME="$home" XDG_DATA_HOME="relative/data" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
assert_contains "relative XDG_DATA_HOME triggers a warning" "$warn" \
    "ignoring relative XDG_DATA_HOME"
assert_file_exists "desktop entry falls back to the default data home" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
rm -rf -- "$work"

echo "=== test: a pre-existing symlink at a desktop/icon destination is foreign ==="
# A symlink at a path this installer owns is never written through *and*,
# under the ownership model, is never silently replaced either - it is
# exactly the "same-name symlink pointing somewhere unrelated" case the
# audit named. Default behavior: left alone, warned about. --replace-foreign:
# the symlink itself is moved aside (never followed) and a real file
# installed in its place. The canary the symlink points at must survive
# either way.
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/applications" \
    "$home/.local/share/icons/hicolor/256x256/apps"
bin_dir="$work/bin"

canary="$work/canary"
printf 'canary contents\n' >"$canary"
ln -s -- "$canary" "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
ln -s -- "$canary" \
    "$home/.local/share/icons/hicolor/256x256/apps/io.github.kiehntre.emuwiz.png"

assert_failure "install without --replace-foreign exits non-zero (something was left foreign)" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
canary_content=$(cat "$canary")
if [ "$canary_content" = "canary contents" ]; then
    ok "symlink target was never written through"
else
    bad "symlink target was never written through (canary was modified)"
fi
if [ -L "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop" ]; then
    ok "desktop entry symlink was left in place without --replace-foreign"
else
    bad "desktop entry symlink was left in place without --replace-foreign"
fi
if [ -L "$home/.local/share/icons/hicolor/256x256/apps/io.github.kiehntre.emuwiz.png" ]; then
    ok "icon symlink was left in place without --replace-foreign"
else
    bad "icon symlink was left in place without --replace-foreign"
fi

assert_success "install --replace-foreign succeeds over symlink destinations" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
canary_content=$(cat "$canary")
if [ "$canary_content" = "canary contents" ]; then
    ok "symlink target still untouched after --replace-foreign"
else
    bad "symlink target still untouched after --replace-foreign (canary was modified)"
fi
if [ -L "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop" ]; then
    bad "desktop entry symlink was replaced with a regular file after --replace-foreign"
else
    ok "desktop entry symlink was replaced with a regular file after --replace-foreign"
fi
rm -rf -- "$work"

echo "=== test: an existing legacy config directory is reused ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.config/archivefs"
marker="LEGACY CONFIG - KEEP USING"
printf '%s\n' "$marker" >"$home/.config/archivefs/config.toml"
bin_dir="$work/bin"

assert_success "install with only a legacy config succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
kept=$(cat "$home/.config/archivefs/config.toml")
assert_contains "legacy config content is preserved" "$kept" "$marker"
assert_no_such_path "no EmuWiz config is created beside a legacy profile" \
    "$home/.config/emuwiz/config.toml"
rm -rf -- "$work"

echo "=== test: EmuWiz config takes precedence when both directories exist ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.config/emuwiz" "$home/.config/archivefs"
primary_marker="PRIMARY CONFIG - KEEP USING"
legacy_marker="LEGACY CONFIG - DO NOT SELECT"
printf '%s\n' "$primary_marker" >"$home/.config/emuwiz/config.toml"
printf '%s\n' "$legacy_marker" >"$home/.config/archivefs/config.toml"
bin_dir="$work/bin"

assert_success "install with both config directories succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
kept=$(cat "$home/.config/emuwiz/config.toml")
assert_contains "EmuWiz config wins and is preserved" "$kept" "$primary_marker"
legacy_kept=$(cat "$home/.config/archivefs/config.toml")
assert_contains "unselected legacy config is untouched" "$legacy_kept" "$legacy_marker"
rm -rf -- "$work"

echo "=== test: install from a workspace checkout (target/release) ==="
work=$(mktemp -d)
make_workspace "$work/workspace"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

assert_success "install from workspace succeeds" \
    env HOME="$home" sh "$work/workspace/install.sh" --prefix "$bin_dir"
cli_out=$("$bin_dir/emuwiz-cli")
assert_contains "installed emuwiz-cli runs (workspace stub)" "$cli_out" "ws-cli"
rm -rf -- "$work"

echo "=== test: an existing config is never overwritten ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
marker="USER EDITED - DO NOT CLOBBER"
printf '%s\n' "$marker" >"$home/.config/emuwiz/config.toml"

assert_success "reinstall over an existing config succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
kept=$(cat "$home/.config/emuwiz/config.toml")
assert_contains "existing config content is preserved" "$kept" "$marker"
rm -rf -- "$work"

echo "=== test: uninstall precisely removes owned binaries and desktop assets ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
# A file install.sh did not create must survive uninstall untouched.
printf 'unrelated\n' >"$bin_dir/some-other-tool"
mkdir -p -- "$home/.local/share/applications" \
    "$home/.local/share/icons/hicolor/64x64/apps"
printf 'unrelated desktop\n' >"$home/.local/share/applications/unrelated.desktop"
printf 'unrelated icon\n' >"$home/.local/share/icons/hicolor/64x64/apps/unrelated.png"

assert_success "uninstall succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir"
assert_no_such_path "emuwiz-cli removed" "$bin_dir/emuwiz-cli"
assert_no_such_path "archivefs-cli removed" "$bin_dir/archivefs-cli"
assert_no_such_path "emuwiz removed" "$bin_dir/emuwiz"
assert_no_such_path "emuwiz-gui alias removed" "$bin_dir/emuwiz-gui"
assert_no_such_path "archivefs-gui alias removed" "$bin_dir/archivefs-gui"
assert_no_such_path "EmuWiz desktop entry removed" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
for size in 32 64 128 256 512; do
    assert_no_such_path "$size pixel EmuWiz application icon removed" \
        "$home/.local/share/icons/hicolor/${size}x${size}/apps/io.github.kiehntre.emuwiz.png"
done
assert_file_exists "unrelated file in bin dir is untouched" "$bin_dir/some-other-tool"
assert_file_exists "unrelated desktop entry is untouched" \
    "$home/.local/share/applications/unrelated.desktop"
assert_file_exists "unrelated icon is untouched" \
    "$home/.local/share/icons/hicolor/64x64/apps/unrelated.png"
assert_file_exists "config.toml survives uninstall" "$home/.config/emuwiz/config.toml"
assert_success "uninstalling again is a no-op, not an error" \
    env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir"
rm -rf -- "$work"

echo "=== test: uninstall keeps going when an owned path is a directory ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
# Something has occupied the desktop entry's path with a directory instead
# of the file install.sh wrote there (broken package state, another tool,
# a prior crashed run, etc).
rm -f -- "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
mkdir -p -- "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" 2>&1 1>/dev/null)
ok "uninstall did not abort when the desktop entry path is a directory"
assert_contains "uninstall warns about the unexpected directory" "$warn" \
    "found a directory"
assert_no_such_path "emuwiz-cli is still removed despite the desktop-entry collision" \
    "$bin_dir/emuwiz-cli"
for size in 32 64 128 256 512; do
    assert_no_such_path "$size pixel icon is still removed despite the desktop-entry collision" \
        "$home/.local/share/icons/hicolor/${size}x${size}/apps/io.github.kiehntre.emuwiz.png"
done
if [ -d "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop" ]; then
    ok "the colliding directory itself was left in place"
else
    bad "the colliding directory itself was left in place"
fi
rm -rf -- "$work"

echo "=== test: fails clearly when required binaries are missing ==="
work=$(mktemp -d)
empty_dir="$work/empty"
mkdir -p -- "$empty_dir"
cp -- "$install_sh" "$empty_dir/install.sh"
home="$work/home"
mkdir -p -- "$home"
errfile=$(mktemp)

if env HOME="$home" sh "$empty_dir/install.sh" --prefix "$work/bin" >/dev/null 2>"$errfile"; then
    bad "install fails when no binaries are present"
else
    ok "install fails when no binaries are present"
fi
missing_err=$(cat "$errfile")
assert_contains "failure message names the missing binaries" "$missing_err" "emuwiz-cli"
rm -f -- "$errfile"
rm -rf -- "$work"

echo "=== test: warns when ratarmount is not on PATH ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

# A minimal PATH that has core utilities but almost certainly not
# ratarmount (which is normally pip/AppImage-installed under a user or
# venv directory, not /usr/bin or /bin).
warn=$(env HOME="$home" PATH="/usr/bin:/bin" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
assert_contains "prints a ratarmount warning" "$warn" "ratarmount was not found"
assert_contains "warning includes install guidance" "$warn" "pip install ratarmount"
rm -rf -- "$work"

echo "=== test: does not modify shell startup files ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
for rc in .bashrc .profile .zshrc; do
    printf 'original contents\n' >"$home/$rc"
done

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$work/bin" >/dev/null

startup_files_ok=1
for rc in .bashrc .profile .zshrc; do
    rc_content=$(cat "$home/$rc")
    if [ "$rc_content" != "original contents" ]; then
        startup_files_ok=0
    fi
done
if [ "$startup_files_ok" -eq 1 ]; then
    ok "shell startup files are untouched"
else
    bad "shell startup files are untouched"
fi
rm -rf -- "$work"

# ===========================================================================
# Ownership manifest: install/reinstall/uninstall collision safety.
# ===========================================================================

manifest_path() {
    printf '%s/emuwiz-installer/manifest\n' "$1"
}

echo "=== test: fresh install writes an ownership manifest for every slot ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")
assert_file_exists "install creates an ownership manifest" "$manifest"
manifest_content=$(cat "$manifest")
assert_contains "manifest records schema_version" "$manifest_content" "schema_version 2"
assert_contains "manifest records the resolved bin_dir" "$manifest_content" "bin_dir $bin_dir"
assert_contains "manifest records a record_count" "$manifest_content" "record_count 11"
assert_contains "manifest ends with the end marker" "$manifest_content" "
end"
for slot in bin-emuwiz-cli bin-emuwiz alias-archivefs-cli alias-emuwiz-gui \
    alias-archivefs-gui desktop icon-32 icon-64 icon-128 icon-256 icon-512; do
    assert_contains "manifest records slot $slot" "$manifest_content" "$slot "
done
for slot in bin-emuwiz-cli bin-emuwiz desktop icon-32 icon-64 icon-128 icon-256 icon-512; do
    digest_line=$(grep "^$slot file " "$manifest")
    digest_field=$(printf '%s\n' "$digest_line" | awk '{print $3}')
    if [ "${#digest_field}" -eq 64 ]; then
        case "$digest_field" in
            *[!0-9a-f]*) bad "slot $slot records a 64-character lowercase-hex SHA-256 digest (got: $digest_field)" ;;
            *) ok "slot $slot records a 64-character lowercase-hex SHA-256 digest" ;;
        esac
    else
        bad "slot $slot records a 64-character lowercase-hex SHA-256 digest (wrong length: $digest_field)"
    fi
done
slot_lines=$(grep -vc '^#' "$manifest")
# 4 header lines (schema_version, bin_dir, data_home, record_count) + 11
# slots + 1 end marker = 16.
if [ "$slot_lines" -eq 16 ]; then
    ok "manifest has exactly 16 non-comment lines (4 header + 11 slots + end)"
else
    bad "manifest has exactly 16 non-comment lines (4 header + 11 slots + end) (got $slot_lines)"
fi
perm=$(stat -c '%a' "$manifest" 2>/dev/null || stat -f '%Lp' "$manifest" 2>/dev/null)
if [ "$perm" = "600" ]; then
    ok "manifest is written with mode 600"
else
    bad "manifest is written with mode 600 (got $perm)"
fi
rm -rf -- "$work"

echo "=== test: reinstall against its own manifest prints no foreign warnings ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
reinstall_err=$(env HOME="$home" PATH="$fake_ratarmount_dir:$PATH" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
if [ -z "$reinstall_err" ]; then
    ok "reinstall against a matching manifest produces no warnings"
else
    bad "reinstall against a matching manifest produces no warnings (got: $reinstall_err)"
fi
assert_success "reinstall against a matching manifest exits 0" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
rm -rf -- "$work"

echo "=== test: a foreign binary at the destination is not overwritten ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home" "$work/bin"
bin_dir="$work/bin"

foreign_marker="totally-unrelated-tool-$$"
printf '#!/bin/sh\necho %s\n' "$foreign_marker" >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns about the foreign binary" "$warn" \
    "leaving foreign path untouched"
assert_contains "warning names --replace-foreign" "$warn" "--replace-foreign"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "foreign binary content is unchanged" "$foreign_content" "$foreign_marker"
assert_failure "install without --replace-foreign exits non-zero when something was skipped" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
# Everything else must still have installed normally around the collision.
assert_executable "emuwiz (the other binary) still installed" "$bin_dir/emuwiz"
assert_file_exists "desktop entry still installed despite the binary collision" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"

assert_success "install --replace-foreign succeeds over the foreign binary" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
cli_out=$("$bin_dir/emuwiz-cli")
assert_contains "emuwiz-cli now runs the real installed stub" "$cli_out" "fake-cli"
backup_found=0
for f in "$bin_dir"/.emuwiz-foreign-backup.*/emuwiz-cli; do
    [ -f "$f" ] || continue
    backup_found=1
    backup_content=$(cat "$f")
    assert_contains "foreign-backup file preserves the original content" "$backup_content" "$foreign_marker"
done
if [ "$backup_found" -eq 1 ]; then
    ok "--replace-foreign left a recoverable backup of the foreign binary"
else
    bad "--replace-foreign left a recoverable backup of the foreign binary"
fi
rm -rf -- "$work"

echo "=== test: a foreign desktop entry is not overwritten ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/applications"
bin_dir="$work/bin"

desktop_file="$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
printf '[Desktop Entry]\nType=Application\nName=NotEmuWiz\n' >"$desktop_file"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns about the foreign desktop entry" "$warn" \
    "leaving foreign path untouched"
desktop_content=$(cat "$desktop_file")
assert_contains "foreign desktop entry content is unchanged" "$desktop_content" "NotEmuWiz"
assert_executable "emuwiz-cli still installed despite the desktop-entry collision" "$bin_dir/emuwiz-cli"

assert_success "install --replace-foreign succeeds over the foreign desktop entry" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
desktop_content=$(cat "$desktop_file")
assert_contains "desktop entry now has real EmuWiz content" "$desktop_content" \
    "Icon=io.github.kiehntre.emuwiz"
rm -rf -- "$work"

echo "=== test: a foreign icon is not overwritten ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/icons/hicolor/32x32/apps"
bin_dir="$work/bin"

icon_file="$home/.local/share/icons/hicolor/32x32/apps/io.github.kiehntre.emuwiz.png"
printf 'not a real png\n' >"$icon_file"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns about the foreign icon" "$warn" "leaving foreign path untouched"
icon_content=$(cat "$icon_file")
assert_contains "foreign icon content is unchanged" "$icon_content" "not a real png"
assert_files_equal "the other 4 icon sizes still installed despite the 32px collision" \
    "$branding_assets/emuwiz-logo-64.png" \
    "$home/.local/share/icons/hicolor/64x64/apps/io.github.kiehntre.emuwiz.png"

assert_success "install --replace-foreign succeeds over the foreign icon" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
assert_files_equal "32px icon now matches the approved asset" \
    "$branding_assets/emuwiz-logo-32.png" "$icon_file"
rm -rf -- "$work"

echo "=== test: a symlink alias pointing at the wrong target is foreign ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home" "$work/bin"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
rm -f -- "$bin_dir/archivefs-cli"
ln -s -- /etc/passwd "$bin_dir/archivefs-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns about the wrong-target alias symlink" "$warn" \
    "leaving foreign path untouched"
target=$(readlink -- "$bin_dir/archivefs-cli")
if [ "$target" = /etc/passwd ]; then
    ok "wrong-target alias symlink was left exactly as it was"
else
    bad "wrong-target alias symlink was left exactly as it was (target now: $target)"
fi

assert_success "install --replace-foreign fixes the wrong-target alias" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
target=$(readlink -- "$bin_dir/archivefs-cli")
if [ "$target" = emuwiz-cli ]; then
    ok "alias symlink now points at the correct target"
else
    bad "alias symlink now points at the correct target (got: $target)"
fi
rm -rf -- "$work"

echo "=== test: a stale manifest does not permit deleting a replaced foreign binary (same second, no sleep) ==="
# Deliberately no sleep anywhere in this test: content is the only
# ownership authority now, so a same-second, same-mtime replacement must
# be caught exactly as reliably as one seconds apart.
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
owned_stamp_ref="$bin_dir/emuwiz"
rm -f -- "$bin_dir/emuwiz-cli"
printf 'foreign replacement\n' >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
# Forge the mtime to be identical, to the second, to another genuinely
# owned file installed in the same run - the strongest same-second forgery
# available without sub-second timers.
touch -r "$owned_stamp_ref" -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" 2>&1 1>/dev/null)
assert_contains "uninstall warns about the replaced binary" "$warn" \
    "leaving foreign path untouched"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "replaced binary content survives uninstall" "$foreign_content" "foreign replacement"
assert_no_such_path "the other, still-owned binary is removed" "$bin_dir/emuwiz"
assert_no_such_path "the desktop entry is removed" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
rm -rf -- "$work"

echo "=== test: touch -r from a genuinely owned file does not launder foreign content (reinstall) ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
owned_ref="$bin_dir/emuwiz"
rm -f -- "$bin_dir/emuwiz-cli"
printf 'forged-mtime foreign content\n' >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
touch -r "$owned_ref" -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install still detects foreign content after touch -r forgery" "$warn" \
    "leaving foreign path untouched"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "touch -r forged foreign binary content is preserved" "$foreign_content" \
    "forged-mtime foreign content"
rm -rf -- "$work"

echo "=== test: touch -d to the exact recorded epoch does not launder foreign content (uninstall) ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
install_time=$(date -r "$bin_dir/emuwiz" +%s 2>/dev/null || stat -c '%Y' "$bin_dir/emuwiz")
rm -f -- "$bin_dir/emuwiz-cli"
printf 'touch-d forged foreign content\n' >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
touch -d "@$install_time" -- "$bin_dir/emuwiz-cli" 2>/dev/null || \
    touch -t "$(date -d "@$install_time" +%Y%m%d%H%M.%S)" -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" 2>&1 1>/dev/null)
assert_contains "uninstall still detects foreign content after touch -d forgery" "$warn" \
    "leaving foreign path untouched"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "touch -d forged foreign binary content survives uninstall" "$foreign_content" \
    "touch-d forged foreign content"
rm -rf -- "$work"

echo "=== test: cp -p (timestamp-preserving replacement) does not launder foreign content ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
foreign_src="$work/foreign-source"
printf 'cp -p forged foreign content\n' >"$foreign_src"
chmod +x -- "$foreign_src"
touch -r "$bin_dir/emuwiz-cli" -- "$foreign_src"
rm -f -- "$bin_dir/emuwiz-cli"
cp -p -- "$foreign_src" "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install still detects foreign content after cp -p replacement" "$warn" \
    "leaving foreign path untouched"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "cp -p replaced foreign binary content is preserved" "$foreign_content" \
    "cp -p forged foreign content"
rm -rf -- "$work"

echo "=== test: digest ownership refreshes correctly across a legitimate content/version upgrade ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
v1_out=$("$bin_dir/emuwiz-cli")
assert_contains "v1 binary runs the v1 stub" "$v1_out" "fake-cli"

# Simulate a new release: the bundle's own binary content changes.
printf '#!/bin/sh\necho fake-cli-v2\n' >"$work/bundle/emuwiz-cli"
chmod +x -- "$work/bundle/emuwiz-cli"

v2_err=$(env HOME="$home" PATH="$fake_ratarmount_dir:$PATH" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
if [ -z "$v2_err" ]; then
    ok "upgrading to v2 content is recognised as a legitimate reinstall (no foreign warning)"
else
    bad "upgrading to v2 content is recognised as a legitimate reinstall (no foreign warning) (got: $v2_err)"
fi
v2_out=$("$bin_dir/emuwiz-cli")
assert_contains "binary now runs the v2 stub" "$v2_out" "fake-cli-v2"

# A further reinstall against the NEW (v2) recorded digest must also be silent -
# proving the manifest was actually refreshed to the new content's digest,
# not left pointing at v1's.
v2_again_err=$(env HOME="$home" PATH="$fake_ratarmount_dir:$PATH" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
if [ -z "$v2_again_err" ]; then
    ok "manifest was refreshed to the v2 digest (a further v2 reinstall is silent)"
else
    bad "manifest was refreshed to the v2 digest (a further v2 reinstall is silent) (got: $v2_again_err)"
fi
rm -rf -- "$work"

echo "=== test: a malformed manifest fails safe (treated as no manifest) ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")
printf 'garbage this is not a valid manifest !!! ***\n\x00\x01binary junk\n' >"$manifest"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "malformed manifest causes binaries to be treated as foreign (fail safe)" \
    "$warn" "leaving foreign path untouched"
assert_success "install still completes (does not crash) with a malformed manifest" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
rm -rf -- "$work"

echo "=== test: a truncated manifest fails safe ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")
head -c 25 "$manifest" >"$work/truncated"
mv -- "$work/truncated" "$manifest"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "truncated manifest causes binaries to be treated as foreign (fail safe)" \
    "$warn" "leaving foreign path untouched"
rm -rf -- "$work"

echo "=== test: an adversarial manifest cannot make uninstall touch an arbitrary path ==="
# Both sub-cases below keep the header (schema_version/bin_dir/data_home)
# perfectly valid, singular, and matching this exact run, so the parser
# runs past header validation and actually reaches - reads, and evaluates
# the key of - the injected malicious line itself, rather than rejecting
# the whole manifest earlier for an unrelated header mismatch. Duplicate-
# header rejection is covered separately and explicitly by the schema
# parser test battery below.
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")

canary="$work/outside-canary"
printf 'must never be touched\n' >"$canary"

# Sub-case 1: an unknown key naming an arbitrary path, spliced in among the
# otherwise-valid slot records (before the "end" marker, not appended after
# a header mismatch). install.sh never reads a path out of the manifest at
# all - only the fixed, closed set of slot names is ever consulted - so
# there is no key this could ever be that would cause anything to be read
# from or written to $canary.
sed '$d' "$manifest" >"$work/adversarial-1"
printf 'evil-path %s\n' "$canary" >>"$work/adversarial-1"
printf 'end\n' >>"$work/adversarial-1"
cp -- "$work/adversarial-1" "$manifest"

env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
canary_content=$(cat "$canary")
assert_contains "canary survives an adversarial manifest with an injected unknown key" \
    "$canary_content" "must never be touched"
assert_executable "the binary is left in place too (manifest was rejected outright, so it looks foreign)" \
    "$bin_dir/emuwiz"

# Reinstall clean before sub-case 2, so there is a fresh, valid manifest to
# adversarially edit again.
env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign >/dev/null
manifest=$(manifest_path "$home/.local/share")

# Sub-case 2: a slot's "fingerprint" field replaced with a path string
# instead of a valid 64-character hex digest - the field-shape validation
# itself (validate_file_digest) is what must reject this, reached and
# evaluated at that specific slot's own line.
sed "s|^bin-emuwiz-cli file .*|bin-emuwiz-cli file $canary 4|" "$manifest" >"$work/adversarial-2"
cp -- "$work/adversarial-2" "$manifest"

env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
canary_content=$(cat "$canary")
assert_contains "canary survives an adversarial manifest with a path-shaped digest field" \
    "$canary_content" "must never be touched"
assert_executable "the binary is left in place too (path-shaped digest was rejected, so it looks foreign)" \
    "$bin_dir/emuwiz"
rm -rf -- "$work"

echo "=== test: uninstall preserves a foreign replacement while removing everything else ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
rm -f -- "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
printf 'foreign desktop replacement\n' \
    >"$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"

env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
desktop_content=$(cat "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop")
assert_contains "foreign desktop replacement survives uninstall" "$desktop_content" \
    "foreign desktop replacement"
assert_no_such_path "binaries are still removed" "$bin_dir/emuwiz-cli"
for size in 32 64 128 256 512; do
    assert_no_such_path "$size px icon is still removed" \
        "$home/.local/share/icons/hicolor/${size}x${size}/apps/io.github.kiehntre.emuwiz.png"
done
assert_file_exists "the manifest is kept (something was left foreign)" \
    "$(manifest_path "$home/.local/share")"
rm -rf -- "$work"

echo "=== test: a clean uninstall removes its own bookkeeping manifest ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null
assert_no_such_path "manifest is removed after a fully clean uninstall" \
    "$(manifest_path "$home/.local/share")"
rm -rf -- "$work"

echo "=== test: uninstall is idempotent even after a foreign collision was left behind ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
rm -f -- "$bin_dir/emuwiz-cli"
printf 'foreign\n' >"$bin_dir/emuwiz-cli"
env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
assert_success "uninstalling a second time after a foreign collision is still not an error" \
    env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "foreign binary is still there and still untouched" "$foreign_content" "foreign"
rm -rf -- "$work"

echo "=== test: a directory at a binary destination is left alone on install ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home" "$work/bin/emuwiz-cli"
bin_dir="$work/bin"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns about the directory collision" "$warn" \
    "leaving foreign path untouched"
if [ -d "$bin_dir/emuwiz-cli" ] && [ ! -L "$bin_dir/emuwiz-cli" ]; then
    ok "the colliding directory at the binary destination is untouched"
else
    bad "the colliding directory at the binary destination is untouched"
fi
assert_executable "emuwiz (the other binary) still installed despite the collision" \
    "$bin_dir/emuwiz"
rm -rf -- "$work"

echo "=== test: reinstall after a partial (never-manifested) install recovers safely ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

# Simulate a run that was killed after copying the binaries but before the
# manifest was ever written: no manifest exists, but a plain, genuinely
# EmuWiz-installed binary already sits at the destination.
mkdir -p -- "$bin_dir"
cp -- "$work/bundle/emuwiz-cli" "$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "the interrupted run's own binary is treated as foreign, not silently claimed" \
    "$warn" "leaving foreign path untouched"
assert_success "--replace-foreign completes the interrupted install" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
assert_file_exists "manifest now exists after the completed reinstall" \
    "$(manifest_path "$home/.local/share")"
assert_success "a further reinstall is now silent and clean" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
rm -rf -- "$work"

echo "=== test: legacy aliases still resolve to the real binaries after ownership tracking ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
cli_out=$("$bin_dir/archivefs-cli")
assert_contains "archivefs-cli alias still runs the real emuwiz-cli" "$cli_out" "fake-cli"
gui_target=$(readlink -- "$bin_dir/emuwiz-gui")
[ "$gui_target" = emuwiz ] && ok "emuwiz-gui alias points at emuwiz" || bad "emuwiz-gui alias points at emuwiz"
gui_target=$(readlink -- "$bin_dir/archivefs-gui")
[ "$gui_target" = emuwiz ] && ok "archivefs-gui alias points at emuwiz" || bad "archivefs-gui alias points at emuwiz"
rm -rf -- "$work"

# ===========================================================================
# --replace-foreign backup safety: exclusive allocation, no clobbering, no
# symlink-following, real mid-install failure recovery.
# ===========================================================================

echo "=== test: pre-existing decoys near the backup naming prefix do not interfere with --replace-foreign ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home" "$work/bin"
bin_dir="$work/bin"

foreign_marker="decoy-test-foreign-$$"
printf '#!/bin/sh\necho %s\n' "$foreign_marker" >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"

# Plant a regular file, a symlink, and a directory that all share the
# static ".emuwiz-foreign-backup." prefix this installer uses - the closest
# an attacker without the ability to predict mktemp's random suffix could
# plausibly pre-stage. None of these are the actual backup mktemp -d will
# allocate (its suffix is unpredictable), so none of them should be
# touched, read through, or interfered with.
printf 'decoy regular file - must survive\n' >"$bin_dir/.emuwiz-foreign-backup.decoyfile"
ln -s -- /etc/passwd "$bin_dir/.emuwiz-foreign-backup.decoylink"
mkdir -p -- "$bin_dir/.emuwiz-foreign-backup.decoydir"
printf 'decoy directory contents - must survive\n' >"$bin_dir/.emuwiz-foreign-backup.decoydir/canary"

assert_success "install --replace-foreign succeeds despite backup-prefix decoys" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign
assert_contains "decoy regular file is untouched" \
    "$(cat "$bin_dir/.emuwiz-foreign-backup.decoyfile")" "must survive"
decoy_link_target=$(readlink -- "$bin_dir/.emuwiz-foreign-backup.decoylink")
[ "$decoy_link_target" = /etc/passwd ] \
    && ok "decoy symlink still points where it always did (never followed)" \
    || bad "decoy symlink still points where it always did (never followed) (got: $decoy_link_target)"
assert_contains "decoy directory contents are untouched" \
    "$(cat "$bin_dir/.emuwiz-foreign-backup.decoydir/canary")" "must survive"

real_backup_found=0
for d in "$bin_dir"/.emuwiz-foreign-backup.*; do
    case "$d" in
        *decoyfile|*decoylink|*decoydir) continue ;;
    esac
    [ -d "$d" ] || continue
    real_backup_found=1
    backup_content=$(cat "$d/emuwiz-cli")
    assert_contains "the real (non-decoy) backup preserves the original foreign content" \
        "$backup_content" "$foreign_marker"
done
if [ "$real_backup_found" -eq 1 ]; then
    ok "a distinct, non-colliding backup directory was allocated alongside the decoys"
else
    bad "a distinct, non-colliding backup directory was allocated alongside the decoys"
fi
rm -rf -- "$work"

echo "=== test: multiple simultaneous foreign collisions each get their own distinct backup ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/applications" "$work/bin"
bin_dir="$work/bin"

bin_marker="multi-foreign-binary-$$"
printf '#!/bin/sh\necho %s\n' "$bin_marker" >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
desktop_file="$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
printf '[Desktop Entry]\nType=Application\nName=MultiForeign\n' >"$desktop_file"

assert_success "install --replace-foreign resolves two simultaneous collisions in one run" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign

bin_backup_content=""
for d in "$bin_dir"/.emuwiz-foreign-backup.*; do
    [ -f "$d/emuwiz-cli" ] || continue
    bin_backup_content=$(cat "$d/emuwiz-cli")
done
assert_contains "the binary's own backup preserves its original content" \
    "$bin_backup_content" "$bin_marker"

desktop_backup_content=""
for d in "$home/.local/share/applications"/.emuwiz-foreign-backup.*; do
    [ -f "$d/io.github.kiehntre.emuwiz.desktop" ] || continue
    desktop_backup_content=$(cat "$d/io.github.kiehntre.emuwiz.desktop")
done
assert_contains "the desktop entry's own backup preserves its original content" \
    "$desktop_backup_content" "MultiForeign"

new_desktop_content=$(cat "$desktop_file")
assert_contains "the real desktop entry now has genuine EmuWiz content" \
    "$new_desktop_content" "Icon=io.github.kiehntre.emuwiz"
cli_out=$("$bin_dir/emuwiz-cli")
assert_contains "the real binary now runs the genuine stub" "$cli_out" "fake-cli"
rm -rf -- "$work"

echo "=== test: a real mid-install failure after a successful backup preserves the foreign object ==="
# Genuinely induced, not hand-constructed: the bundle's own source binary
# is made unreadable so cp actually fails partway through
# install_binary_slot, for real, right after backup_foreign_path has
# already succeeded and moved the foreign content aside - exercising the
# exact "backed up, then the next step fails" ordering rather than
# simulating its end state.
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home" "$work/bin"
bin_dir="$work/bin"

foreign_marker="pre-failure-foreign-$$"
printf '#!/bin/sh\necho %s\n' "$foreign_marker" >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
chmod 000 -- "$work/bundle/emuwiz-cli"

fail_out=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign 2>&1 1>/dev/null) || fail_status=$?
chmod 755 -- "$work/bundle/emuwiz-cli"
if [ "${fail_status:-0}" -ne 0 ]; then
    ok "the genuinely-induced cp failure aborts the run with a non-zero exit"
else
    bad "the genuinely-induced cp failure aborts the run with a non-zero exit"
fi
assert_contains "the backup location was reported before the crash (deterministic recovery instruction)" \
    "$fail_out" "moved foreign path aside before installing"
assert_no_such_path "the destination is left absent, not half-written" "$bin_dir/emuwiz-cli"

backup_dir_found=""
for d in "$bin_dir"/.emuwiz-foreign-backup.*; do
    [ -f "$d/emuwiz-cli" ] || continue
    backup_dir_found=$d
done
if [ -n "$backup_dir_found" ]; then
    ok "the foreign object survives the crash, sitting exactly where it was backed up"
    backup_content=$(cat "$backup_dir_found/emuwiz-cli")
    assert_contains "the surviving backup still holds the original foreign content" \
        "$backup_content" "$foreign_marker"
else
    bad "the foreign object survives the crash, sitting exactly where it was backed up"
fi

# Recovery: the destination is absent now (the foreign object moved away,
# the new copy never landed), so a plain rerun - no --replace-foreign
# needed - completes the interrupted install cleanly, and the backup from
# the failed run is still there afterwards, never auto-deleted.
assert_success "a plain rerun after fixing the underlying cause completes cleanly" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
cli_out=$("$bin_dir/emuwiz-cli")
assert_contains "the real binary is now installed" "$cli_out" "fake-cli"
if [ -f "$backup_dir_found/emuwiz-cli" ]; then
    ok "the pre-crash backup is still there after recovery (nothing auto-deletes it)"
else
    bad "the pre-crash backup is still there after recovery (nothing auto-deletes it)"
fi
recovered_content=$(cat "$backup_dir_found/emuwiz-cli")
assert_contains "the pre-crash backup content is still exactly recoverable" \
    "$recovered_content" "$foreign_marker"
rm -rf -- "$work"

# ===========================================================================
# Manifest-directory and manifest-file safety: the bookkeeping path itself
# is part of the security boundary.
# ===========================================================================

echo "=== test: the installer bookkeeping directory as a symlink is refused, and outside canaries survive ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share"
bin_dir="$work/bin"

outside="$work/outside-target"
mkdir -p -- "$outside"
printf 'must never be touched\n' >"$outside/canary.txt"
ln -s -- "$outside" "$home/.local/share/emuwiz-installer"

assert_failure "install refuses outright when the bookkeeping directory is a symlink" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
assert_no_such_path "nothing was installed at all (refusal is total, not partial)" \
    "$bin_dir/emuwiz-cli"
canary_content=$(cat "$outside/canary.txt")
assert_contains "the outside canary survives byte-for-byte" "$canary_content" "must never be touched"
if [ -L "$home/.local/share/emuwiz-installer" ]; then
    ok "the symlink itself was left exactly as it was (never replaced, never traversed-through)"
else
    bad "the symlink itself was left exactly as it was (never replaced, never traversed-through)"
fi
outside_listing=$(ls -a -- "$outside")
assert_contains "nothing new was created inside the symlink target" "$outside_listing" "canary.txt"
rm -rf -- "$work"

echo "=== test: uninstall also refuses to touch a symlinked bookkeeping directory ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
outside="$work/outside-target-2"
mkdir -p -- "$outside"
printf 'must never be touched either\n' >"$outside/canary.txt"
rm -rf -- "$home/.local/share/emuwiz-installer"
ln -s -- "$outside" "$home/.local/share/emuwiz-installer"

env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
canary_content=$(cat "$outside/canary.txt")
assert_contains "the outside canary survives uninstall too" "$canary_content" "must never be touched either"
if [ -L "$home/.local/share/emuwiz-installer" ]; then
    ok "uninstall left the symlinked bookkeeping path exactly as it was"
else
    bad "uninstall left the symlinked bookkeeping path exactly as it was"
fi
# With the manifest unreachable (treated as no manifest), binaries have no
# fallback recognition and are correctly left in place, not removed.
assert_executable "binaries are left in place (manifest unreachable, so they look foreign)" \
    "$bin_dir/emuwiz-cli"
rm -rf -- "$work"

echo "=== test: an existing manifest that is a foreign regular file is left untouched ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/emuwiz-installer"
bin_dir="$work/bin"

manifest=$(manifest_path "$home/.local/share")
printf 'this file belongs to something else entirely\n' >"$manifest"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns that the manifest itself looks foreign" "$warn" \
    "leaving the ownership manifest untouched"
manifest_content=$(cat "$manifest")
assert_contains "the foreign manifest file content is preserved exactly" \
    "$manifest_content" "this file belongs to something else entirely"
assert_executable "the binaries still installed normally around the manifest collision" \
    "$bin_dir/emuwiz-cli"
rm -rf -- "$work"

echo "=== test: an existing manifest that is a symlink is left untouched (never mv'd through) ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/emuwiz-installer"
bin_dir="$work/bin"

manifest=$(manifest_path "$home/.local/share")
outside_target="$work/manifest-symlink-target"
printf 'must never be overwritten via the manifest symlink\n' >"$outside_target"
ln -s -- "$outside_target" "$manifest"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns that the manifest symlink looks foreign" "$warn" \
    "leaving the ownership manifest untouched"
if [ -L "$manifest" ]; then
    ok "the manifest symlink itself is untouched"
else
    bad "the manifest symlink itself is untouched"
fi
target_content=$(cat "$outside_target")
assert_contains "whatever the manifest symlink pointed at is untouched" \
    "$target_content" "must never be overwritten via the manifest symlink"
assert_executable "the binaries still installed normally around the manifest collision" \
    "$bin_dir/emuwiz-cli"
rm -rf -- "$work"

echo "=== test: an existing manifest path that is a directory is left untouched ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/emuwiz-installer"
bin_dir="$work/bin"

manifest=$(manifest_path "$home/.local/share")
mkdir -p -- "$manifest"
printf 'must never be touched\n' >"$manifest/some-file-inside"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "install warns that the manifest-shaped directory looks foreign" "$warn" \
    "leaving the ownership manifest untouched"
if [ -d "$manifest" ] && [ ! -L "$manifest" ]; then
    ok "the directory occupying the manifest path is untouched"
else
    bad "the directory occupying the manifest path is untouched"
fi
inside_content=$(cat "$manifest/some-file-inside")
assert_contains "contents inside that directory are untouched" \
    "$inside_content" "must never be touched"
assert_executable "the binaries still installed normally around the manifest collision" \
    "$bin_dir/emuwiz-cli"
rm -rf -- "$work"

echo "=== test: --replace-foreign recovers a foreign manifest location, backing it up first ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home/.local/share/emuwiz-installer"
bin_dir="$work/bin"

manifest=$(manifest_path "$home/.local/share")
printf 'unrelated content that predates ownership tracking\n' >"$manifest"

assert_failure "install without --replace-foreign leaves something unrecorded (exits non-zero)" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
manifest_content=$(cat "$manifest")
assert_contains "the foreign manifest content is still there before --replace-foreign" \
    "$manifest_content" "unrelated content that predates ownership tracking"

replace_warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign 2>&1 1>/dev/null)
assert_contains "install --replace-foreign reports moving the foreign manifest aside" \
    "$replace_warn" "moved an unrecognised path at the ownership manifest location aside"
assert_success "install --replace-foreign succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign

backup_found=0
for d in "$home/.local/share/emuwiz-installer"/.emuwiz-foreign-backup.*; do
    [ -f "$d/manifest" ] || continue
    backup_found=1
    backup_content=$(cat "$d/manifest")
    assert_contains "the backed-up foreign manifest content is fully recoverable" \
        "$backup_content" "unrelated content that predates ownership tracking"
done
if [ "$backup_found" -eq 1 ]; then
    ok "the foreign manifest was backed up, not deleted"
else
    bad "the foreign manifest was backed up, not deleted"
fi
new_manifest_content=$(cat "$manifest")
assert_contains "a genuine ownership manifest now occupies the path" \
    "$new_manifest_content" "schema_version 2"
assert_success "a further plain reinstall against the new manifest is now silent" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
rm -rf -- "$work"

echo "=== test: a symlinked custom XDG_DATA_HOME is honoured, and unrelated canaries there survive ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
real_data_target="$work/real-data-target"
mkdir -p -- "$real_data_target"
printf 'unrelated pre-existing content - must survive\n' >"$real_data_target/unrelated-canary.txt"
data_home_link="$work/symlinked-data-home"
ln -s -- "$real_data_target" "$data_home_link"
bin_dir="$work/bin"

assert_success "install succeeds against a symlinked XDG_DATA_HOME" \
    env HOME="$home" XDG_DATA_HOME="$data_home_link" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir"
assert_file_exists "the desktop entry landed in the real target the symlink points to" \
    "$real_data_target/applications/io.github.kiehntre.emuwiz.desktop"
assert_file_exists "the manifest landed in the real target the symlink points to" \
    "$real_data_target/emuwiz-installer/manifest"
canary_content=$(cat "$real_data_target/unrelated-canary.txt")
assert_contains "the unrelated pre-existing canary in that real target survives" \
    "$canary_content" "must survive"

assert_success "reinstall against the symlinked XDG_DATA_HOME is silent and clean" \
    env HOME="$home" XDG_DATA_HOME="$data_home_link" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir"
rm -rf -- "$work"

# ===========================================================================
# Manifest parser: fail-closed on any structural ambiguity.
# ===========================================================================

echo "=== test: manifest schema parser rejects structural ambiguity ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")
cp -- "$manifest" "$work/pristine-manifest"

# assert_manifest_rejected DESCRIPTION - installs over the mutation staged
# in $work/mutated-manifest, asserts it was rejected outright (the
# binaries, which have no fallback recognition rule at all, are the
# cleanest possible proof: they warn as foreign if and only if the
# manifest as a whole failed to parse), then reconciles state with
# --replace-foreign before the next sub-test.
assert_manifest_rejected() {
    description=$1
    cp -- "$work/mutated-manifest" "$manifest"
    warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
    assert_contains "$description" "$warn" "leaving foreign path untouched"
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign >/dev/null
}

sed '/^schema_version /{p}' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a duplicate schema_version line invalidates the whole manifest"

sed '/^bin_dir /{p}' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a duplicate bin_dir line invalidates the whole manifest"

sed '/^data_home /{p}' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a duplicate data_home line invalidates the whole manifest"

sed '/^bin-emuwiz-cli /{p}' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a duplicate slot record invalidates the whole manifest"

sed '$d' "$work/pristine-manifest" >"$work/mutated-manifest"
printf 'totally-unknown-key some-value\n' >>"$work/mutated-manifest"
printf 'end\n' >>"$work/mutated-manifest"
assert_manifest_rejected "an unknown field in an otherwise-valid manifest invalidates it"

sed 's/^schema_version .*/schema_version 99/' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "an unsupported schema_version invalidates the manifest"

grep -E '^(schema_version|bin_dir|data_home|record_count) ' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "truncation immediately after a valid header (no records, no end) fails safe"

header_line_count=$(grep -n '^record_count ' "$work/pristine-manifest" | cut -d: -f1)
head -n "$((header_line_count + 5))" "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "truncation between valid slot records (no end, count too high) fails safe"

sed '$d' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a manifest missing the end marker fails safe"

grep -v '^record_count ' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a manifest missing record_count fails safe"

sed '/^record_count /{p}' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "a duplicate record_count line invalidates the whole manifest"

sed 's/^record_count 11/record_count 12/' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "an incorrect record_count (too high, otherwise complete) fails safe"

sed 's/^record_count 11/record_count 10/' "$work/pristine-manifest" >"$work/mutated-manifest"
assert_manifest_rejected "an incorrect record_count (too low, otherwise complete) fails safe"

# The parser contract is that nothing follows "end" - not a record, not a
# header field, and not even a comment. Confirmed on both install (the
# manifest must be rejected outright, exactly as if it had never existed)
# and uninstall (no destructive action may be taken on the strength of a
# manifest with trailing content after its own end marker: the binaries,
# which have zero fallback recognition of their own, must be left in
# place rather than removed).
cat -- "$work/pristine-manifest" >"$work/mutated-manifest"
printf '# trailing comment after end - must invalidate the whole manifest\n' >>"$work/mutated-manifest"
assert_manifest_rejected "a comment appended after the end marker invalidates the manifest (install)"

cp -- "$work/pristine-manifest" "$manifest"
printf '# trailing comment after end - must invalidate the whole manifest\n' >>"$manifest"
uninstall_warn=$(env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" 2>&1 1>/dev/null) || true
assert_contains "uninstall also rejects a manifest with a trailing comment after end" \
    "$uninstall_warn" "leaving foreign path untouched"
assert_executable "uninstall took no destructive action: the binary is still there (looked foreign, correctly not removed)" \
    "$bin_dir/emuwiz-cli"
assert_executable "uninstall took no destructive action: the other binary is still there too" \
    "$bin_dir/emuwiz"
# Recover before the next sub-test.
cp -- "$work/pristine-manifest" "$manifest"

# A valid, intentionally partial manifest IS supported: omit just the
# bin-emuwiz slot's record (adjusting record_count to match) and confirm
# the manifest as a WHOLE is still accepted - bin-emuwiz-cli, still
# recorded, produces no warning at all, while bin-emuwiz, genuinely
# unrecorded in this otherwise-valid manifest, correctly falls back to "no
# recognition rule for binaries" and is reported foreign - proving the
# manifest parsed and was honoured, not merely rejected wholesale.
grep -v '^bin-emuwiz ' "$work/pristine-manifest" | sed 's/^record_count 11/record_count 10/' \
    >"$work/partial-manifest"
cp -- "$work/partial-manifest" "$manifest"
warn=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
if printf '%s\n' "$warn" | grep -Fxq "install.sh: warning: leaving foreign path untouched (not EmuWiz-owned): $bin_dir/emuwiz"; then
    ok "a valid partial manifest is accepted: the un-recorded binary slot is treated as foreign"
else
    bad "a valid partial manifest is accepted: the un-recorded binary slot is treated as foreign (warn: $warn)"
fi
if printf '%s\n' "$warn" | grep -Fq "$bin_dir/emuwiz-cli"; then
    bad "a valid partial manifest is accepted: the still-recorded slot produces no warning at all (it warned anyway)"
else
    ok "a valid partial manifest is accepted: the still-recorded slot produces no warning at all"
fi
env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" --replace-foreign >/dev/null

# A complete, untouched, valid manifest round-trips silently - the
# baseline every mutation above is measured against.
cp -- "$work/pristine-manifest" "$manifest"
complete_err=$(env HOME="$home" PATH="$fake_ratarmount_dir:$PATH" \
    sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null) || true
if [ -z "$complete_err" ]; then
    ok "a complete, valid, unmutated manifest is accepted and produces no warnings"
else
    bad "a complete, valid, unmutated manifest is accepted and produces no warnings (got: $complete_err)"
fi
rm -rf -- "$work"

echo
printf 'Results: %s passed, %s failed\n' "$pass_count" "$fail_count"
[ "$fail_count" -eq 0 ]
