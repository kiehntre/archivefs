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
assert_contains "manifest records schema_version" "$manifest_content" "schema_version 1"
assert_contains "manifest records the resolved bin_dir" "$manifest_content" "bin_dir $bin_dir"
for slot in bin-emuwiz-cli bin-emuwiz alias-archivefs-cli alias-emuwiz-gui \
    alias-archivefs-gui desktop icon-32 icon-64 icon-128 icon-256 icon-512; do
    assert_contains "manifest records slot $slot" "$manifest_content" "$slot "
done
slot_lines=$(grep -vc '^#' "$manifest")
# 3 header lines (schema_version, bin_dir, data_home) + 11 slots = 14.
if [ "$slot_lines" -eq 14 ]; then
    ok "manifest has exactly 14 non-comment lines (3 header + 11 slots)"
else
    bad "manifest has exactly 14 non-comment lines (3 header + 11 slots) (got $slot_lines)"
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
reinstall_err=$(env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" 2>&1 1>/dev/null)
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
for f in "$bin_dir"/emuwiz-cli.foreign-backup.*; do
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

echo "=== test: a stale manifest does not permit deleting a replaced foreign binary ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
sleep 2
rm -f -- "$bin_dir/emuwiz-cli"
printf 'foreign replacement\n' >"$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"

warn=$(env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" 2>&1 1>/dev/null)
assert_contains "uninstall warns about the replaced binary" "$warn" \
    "leaving foreign path untouched"
foreign_content=$(cat "$bin_dir/emuwiz-cli")
assert_contains "replaced binary content survives uninstall" "$foreign_content" "foreign replacement"
assert_no_such_path "the other, still-owned binary is removed" "$bin_dir/emuwiz"
assert_no_such_path "the desktop entry is removed" \
    "$home/.local/share/applications/io.github.kiehntre.emuwiz.desktop"
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
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
manifest=$(manifest_path "$home/.local/share")

canary="$work/outside-canary"
printf 'must never be touched\n' >"$canary"

# Every plausible way an edited manifest might try to point somewhere else:
# an extra unknown key naming an arbitrary path, a slot line whose
# "fingerprint" is itself a path, and a bin_dir override pointing at the
# canary's directory. None of these are ever read as a path by install.sh -
# only bin_dir/data_home (checked for equality, never used to build a path
# beyond what --prefix/$XDG_DATA_HOME already independently resolved) and
# the fixed slot names are ever consulted.
{
    cat -- "$manifest"
    printf 'evil-path %s\n' "$canary"
    printf 'bin-emuwiz-cli file %s\n' "$canary"
    printf 'bin_dir %s\n' "$(dirname -- "$canary")"
} >"$work/adversarial-manifest"
mv -- "$work/adversarial-manifest" "$manifest"

env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir" >/dev/null 2>&1 || true
canary_content=$(cat "$canary")
assert_contains "canary file outside the install footprint survives an adversarial manifest" \
    "$canary_content" "must never be touched"
rm -rf -- "$work"

echo "=== test: uninstall preserves a foreign replacement while removing everything else ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
sleep 2
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
sleep 2
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

echo
printf 'Results: %s passed, %s failed\n' "$pass_count" "$fail_count"
[ "$fail_count" -eq 0 ]
