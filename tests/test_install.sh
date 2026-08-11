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

echo
printf 'Results: %s passed, %s failed\n' "$pass_count" "$fail_count"
[ "$fail_count" -eq 0 ]
