#!/bin/sh
# ArchiveFS installer.
#
# Installs archivefs-cli and archivefs-gui for the current user only: no
# sudo, no system-wide changes, no edits to shell startup files. Safe to
# run more than once - re-running reinstalls the binaries and never
# touches an existing config.
#
# Works from two locations (detected automatically):
#   - an extracted release bundle (archivefs-cli / archivefs-gui sitting
#     next to this script)
#   - a workspace checkout after `cargo build --workspace --release`
#     (target/release/archivefs-cli / target/release/archivefs-gui)
set -eu

program_name="install.sh"
default_bin_dir="$HOME/.local/bin"
config_dir="$HOME/.config/archivefs"
config_file="$config_dir/config.toml"

usage() {
    cat <<EOF
Usage: $program_name [--prefix PATH] [--uninstall] [--help]

Install ArchiveFS (archivefs-cli, archivefs-gui) for the current user.
No sudo is used and no shell startup files are modified.

Options:
  --prefix PATH   Install the binaries into PATH instead of the default
                  ($default_bin_dir). The directory is created if needed.
                  PATH should normally be an absolute path.
  --uninstall     Remove archivefs-cli and archivefs-gui from the install
                  directory (default or --prefix PATH). Your config at
                  $config_file is never touched.
  --help          Show this help and exit.

Without --uninstall, this script:
  1. Detects whether it is running from an extracted release bundle or a
     workspace checkout with release binaries already built, and fails
     with a clear message if neither is found.
  2. Copies archivefs-cli and archivefs-gui into the install directory
     and makes sure they are executable.
  3. Creates $config_dir if it does not exist.
  4. Copies config.toml.example to $config_file, but only if that file
     does not already exist. An existing config is never overwritten.
  5. Checks whether ratarmount is on PATH and prints installation
     guidance if it is not (ArchiveFS uses it to mount archives).
EOF
}

fail() {
    printf '%s: %s\n' "$program_name" "$*" >&2
    exit 1
}

bin_dir="$default_bin_dir"
do_uninstall=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || fail "--prefix requires a PATH argument"
            bin_dir="$2"
            shift 2
            ;;
        --prefix=*)
            bin_dir="${1#--prefix=}"
            shift
            ;;
        --uninstall)
            do_uninstall=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            printf '%s: unknown argument: %s\n' "$program_name" "$1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

[ -n "$bin_dir" ] || fail "--prefix was given an empty PATH"

# Directory containing this script, resolved to an absolute path. Used to
# find the binaries (and config.toml.example) it was shipped alongside.
script_dir=$(dirname -- "$0")
script_dir=$(CDPATH= cd -- "$script_dir" && pwd) || fail "could not resolve the directory containing $program_name"

if [ "$do_uninstall" -eq 1 ]; then
    removed_any=0
    for name in archivefs-cli archivefs-gui; do
        target="$bin_dir/$name"
        if [ -e "$target" ] || [ -L "$target" ]; then
            rm -f -- "$target"
            printf 'Removed %s\n' "$target"
            removed_any=1
        fi
    done
    if [ "$removed_any" -eq 0 ]; then
        printf 'Nothing to uninstall in %s\n' "$bin_dir"
    fi
    printf 'Your configuration at %s was left untouched.\n' "$config_file"
    exit 0
fi

# Prefer an extracted release bundle (binaries next to this script); fall
# back to a workspace checkout with release binaries already built.
if [ -f "$script_dir/archivefs-cli" ] || [ -f "$script_dir/archivefs-gui" ]; then
    src_mode="release bundle"
    src_dir="$script_dir"
elif [ -f "$script_dir/target/release/archivefs-cli" ] || [ -f "$script_dir/target/release/archivefs-gui" ]; then
    src_mode="workspace release build"
    src_dir="$script_dir/target/release"
else
    fail "could not find archivefs-cli or archivefs-gui next to this script ($script_dir) or under target/release/. If you are in a workspace checkout, run 'cargo build --workspace --release' first."
fi

src_cli="$src_dir/archivefs-cli"
src_gui="$src_dir/archivefs-gui"
missing=""
[ -f "$src_cli" ] || missing="$missing archivefs-cli"
[ -f "$src_gui" ] || missing="$missing archivefs-gui"
if [ -n "$missing" ]; then
    fail "missing required binaries in $src_dir:$missing"
fi

mkdir -p -- "$bin_dir"
cp -f -- "$src_cli" "$bin_dir/archivefs-cli"
chmod +x -- "$bin_dir/archivefs-cli"
cp -f -- "$src_gui" "$bin_dir/archivefs-gui"
chmod +x -- "$bin_dir/archivefs-gui"
printf 'Installed archivefs-cli and archivefs-gui to %s (source: %s)\n' "$bin_dir" "$src_mode"

mkdir -p -- "$config_dir"
if [ -e "$config_file" ]; then
    printf 'Existing config found at %s - leaving it untouched.\n' "$config_file"
else
    src_config_example="$script_dir/config.toml.example"
    if [ -f "$src_config_example" ]; then
        cp -- "$src_config_example" "$config_file"
        printf 'Wrote a starter config to %s - edit source_folders and mount_root before running doctor.\n' "$config_file"
    else
        printf '%s: warning: config.toml.example not found next to this script; no starter config was written.\n' "$program_name" >&2
        printf 'Create %s yourself before running archivefs-cli.\n' "$config_file" >&2
    fi
fi

if command -v ratarmount >/dev/null 2>&1; then
    printf 'ratarmount found: %s\n' "$(command -v ratarmount)"
else
    printf '\n' >&2
    printf 'WARNING: ratarmount was not found on PATH.\n' >&2
    printf 'ArchiveFS uses ratarmount to mount archives read-only, and mounting\n' >&2
    printf 'will not work until it is installed.\n' >&2
    printf 'Install it with: pip install ratarmount\n' >&2
    printf '(see https://github.com/mxmlnkn/ratarmount for other options,\n' >&2
    printf 'including a portable AppImage build that needs no installation.)\n' >&2
fi

path_has_bin_dir=0
case ":$PATH:" in
    *":$bin_dir:"*) path_has_bin_dir=1 ;;
esac

printf '\n'
printf 'Next steps:\n'
printf '  1. Edit %s (source_folders, mount_root).\n' "$config_file"
if [ "$path_has_bin_dir" -eq 1 ]; then
    printf '  2. Run: archivefs-cli doctor\n'
    printf '  3. Run: archivefs-cli config-check\n'
    printf '  4. Launch the GUI: archivefs-gui\n'
else
    printf '  2. %s is not on your PATH yet. Add it yourself (this script\n' "$bin_dir"
    printf '     will not edit your shell startup files), for example by adding\n'
    printf '     this line to ~/.bashrc or ~/.zshrc:\n'
    printf '       export PATH="%s:$PATH"\n' "$bin_dir"
    printf '     Until then, use the full path:\n'
    printf '  3. Run: %s/archivefs-cli doctor\n' "$bin_dir"
    printf '  4. Run: %s/archivefs-cli config-check\n' "$bin_dir"
    printf '  5. Launch the GUI: %s/archivefs-gui\n' "$bin_dir"
fi

printf '\n'
printf 'To uninstall later: %s/install.sh --uninstall' "$script_dir"
if [ "$bin_dir" != "$default_bin_dir" ]; then
    printf ' --prefix %s' "$bin_dir"
fi
printf '\n'
