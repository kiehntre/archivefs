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
# install.sh, stub archivefs-cli/archivefs-gui, config.toml.example.
make_bundle() {
    mkdir -p -- "$1"
    cp -- "$install_sh" "$1/install.sh"
    printf '#!/bin/sh\necho fake-cli\n' >"$1/archivefs-cli"
    printf '#!/bin/sh\necho fake-gui\n' >"$1/archivefs-gui"
    chmod +x -- "$1/archivefs-cli" "$1/archivefs-gui"
    cp -- "$config_example" "$1/config.toml.example"
}

# make_workspace DIR - populates DIR with a fake workspace checkout:
# install.sh at the root, stub binaries under target/release/,
# config.toml.example at the root.
make_workspace() {
    mkdir -p -- "$1/target/release"
    cp -- "$install_sh" "$1/install.sh"
    printf '#!/bin/sh\necho ws-cli\n' >"$1/target/release/archivefs-cli"
    printf '#!/bin/sh\necho ws-gui\n' >"$1/target/release/archivefs-gui"
    chmod +x -- "$1/target/release/archivefs-cli" "$1/target/release/archivefs-gui"
    cp -- "$config_example" "$1/config.toml.example"
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
assert_executable "archivefs-cli installed and executable" "$bin_dir/archivefs-cli"
assert_executable "archivefs-gui installed and executable" "$bin_dir/archivefs-gui"
assert_file_exists "config.toml created" "$home/.config/archivefs/config.toml"
assert_files_equal "config.toml matches config.toml.example" \
    "$config_example" "$home/.config/archivefs/config.toml"
cli_out=$("$bin_dir/archivefs-cli")
assert_contains "installed archivefs-cli runs (bundle stub)" "$cli_out" "fake-cli"
rm -rf -- "$work"

echo "=== test: install from a workspace checkout (target/release) ==="
work=$(mktemp -d)
make_workspace "$work/workspace"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

assert_success "install from workspace succeeds" \
    env HOME="$home" sh "$work/workspace/install.sh" --prefix "$bin_dir"
cli_out=$("$bin_dir/archivefs-cli")
assert_contains "installed archivefs-cli runs (workspace stub)" "$cli_out" "ws-cli"
rm -rf -- "$work"

echo "=== test: an existing config is never overwritten ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
marker="USER EDITED - DO NOT CLOBBER"
printf '%s\n' "$marker" >"$home/.config/archivefs/config.toml"

assert_success "reinstall over an existing config succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir"
kept=$(cat "$home/.config/archivefs/config.toml")
assert_contains "existing config content is preserved" "$kept" "$marker"
rm -rf -- "$work"

echo "=== test: uninstall removes only the installed binaries, keeps config ==="
work=$(mktemp -d)
make_bundle "$work/bundle"
home="$work/home"
mkdir -p -- "$home"
bin_dir="$work/bin"

env HOME="$home" sh "$work/bundle/install.sh" --prefix "$bin_dir" >/dev/null
# A file install.sh did not create must survive uninstall untouched.
printf 'unrelated\n' >"$bin_dir/some-other-tool"

assert_success "uninstall succeeds" \
    env HOME="$home" sh "$work/bundle/install.sh" --uninstall --prefix "$bin_dir"
assert_no_such_path "archivefs-cli removed" "$bin_dir/archivefs-cli"
assert_no_such_path "archivefs-gui removed" "$bin_dir/archivefs-gui"
assert_file_exists "unrelated file in bin dir is untouched" "$bin_dir/some-other-tool"
assert_file_exists "config.toml survives uninstall" "$home/.config/archivefs/config.toml"
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
assert_contains "failure message names the missing binaries" "$missing_err" "archivefs-cli"
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
