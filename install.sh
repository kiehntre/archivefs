#!/bin/sh
# EmuWiz installer.
#
# Installs emuwiz-cli and emuwiz for the current user only: no
# sudo, no system-wide changes, no edits to shell startup files. Safe to
# run more than once - re-running reinstalls the binaries and never
# touches an existing config.
#
# Works from two locations (detected automatically):
#   - an extracted release bundle (emuwiz-cli / emuwiz sitting
#     next to this script)
#   - a workspace checkout after `cargo build --workspace --release`
#     (target/release/emuwiz-cli / target/release/emuwiz)
set -eu

program_name="install.sh"
default_bin_dir="$HOME/.local/bin"
if [ -n "${XDG_DATA_HOME:-}" ]; then
    case "$XDG_DATA_HOME" in
        /*) data_home="$XDG_DATA_HOME" ;;
        *)
            printf '%s: warning: ignoring relative XDG_DATA_HOME; using %s/.local/share\n' \
                "$program_name" "$HOME" >&2
            data_home="$HOME/.local/share"
            ;;
    esac
else
    data_home="$HOME/.local/share"
fi
desktop_id="io.github.kiehntre.emuwiz"
desktop_file="$data_home/applications/$desktop_id.desktop"
# EmuWiz config directory, with legacy ArchiveFS reuse: an existing
# `~/.config/archivefs` is honoured so pre-rename settings keep loading;
# a fresh install uses `~/.config/emuwiz`. This mirrors the application's
# own directory resolution.
config_root="$HOME/.config/emuwiz"
if [ ! -e "$config_root" ] && [ ! -L "$config_root" ] \
    && { [ -e "$HOME/.config/archivefs" ] || [ -L "$HOME/.config/archivefs" ]; }; then
    config_root="$HOME/.config/archivefs"
fi
config_dir="$config_root"
config_file="$config_dir/config.toml"

usage() {
    cat <<EOF
Usage: $program_name [--prefix PATH] [--uninstall] [--help]

Install EmuWiz (emuwiz-cli, emuwiz) for the current user.
No sudo is used and no shell startup files are modified.

Options:
  --prefix PATH   Install the binaries into PATH instead of the default
                  ($default_bin_dir). The directory is created if needed.
                  PATH should normally be an absolute path.
  --uninstall     Remove EmuWiz binaries, its desktop entry and application
                  icons. Your config at $config_file is never touched.
  --help          Show this help and exit.

Without --uninstall, this script:
  1. Detects whether it is running from an extracted release bundle or a
     workspace checkout with release binaries already built, and fails
     with a clear message if neither is found.
  2. Copies emuwiz-cli and emuwiz into the install directory
     and makes sure they are executable.
  3. Installs one EmuWiz desktop launcher and its approved application icons
     below $data_home.
  4. Creates $config_dir if it does not exist.
  5. Copies config.toml.example to $config_file, but only if that file
     does not already exist. An existing config is never overwritten.
  6. If a new config was just written and this script is running
     interactively, optionally prompts for one archive source folder to
     add via 'emuwiz-cli source add'. Leave blank to skip - source
     folders are never required at install time, and more can always be
     added later from the Sources page in the GUI or the CLI. Never
     offered for an existing config, and never offered for a
     non-interactive install.
  7. Checks whether ratarmount is on PATH and prints installation
     guidance if it is not (EmuWiz uses it to mount archives).
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
    # Removes the new EmuWiz binaries and the legacy ArchiveFS aliases.
    for name in emuwiz emuwiz-gui emuwiz-cli archivefs-cli archivefs-gui; do
        target="$bin_dir/$name"
        if [ -e "$target" ] || [ -L "$target" ]; then
            rm -f -- "$target"
            printf 'Removed %s\n' "$target"
            removed_any=1
        fi
    done
    if [ -e "$desktop_file" ] || [ -L "$desktop_file" ]; then
        rm -f -- "$desktop_file"
        printf 'Removed %s\n' "$desktop_file"
        removed_any=1
    fi
    for size in 32 64 128 256 512; do
        icon_file="$data_home/icons/hicolor/${size}x${size}/apps/$desktop_id.png"
        if [ -e "$icon_file" ] || [ -L "$icon_file" ]; then
            rm -f -- "$icon_file"
            printf 'Removed %s\n' "$icon_file"
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
# back to a workspace checkout with release binaries already built. The new
# EmuWiz names are preferred, with the legacy ArchiveFS names accepted so
# older release bundles still install.
if [ -f "$script_dir/emuwiz-cli" ] || [ -f "$script_dir/emuwiz" ]; then
    src_mode="release bundle"
    src_dir="$script_dir"
elif [ -f "$script_dir/archivefs-cli" ] || [ -f "$script_dir/archivefs-gui" ]; then
    src_mode="release bundle (legacy names)"
    src_dir="$script_dir"
elif [ -f "$script_dir/target/release/emuwiz-cli" ] || [ -f "$script_dir/target/release/emuwiz" ]; then
    src_mode="workspace release build"
    src_dir="$script_dir/target/release"
elif [ -f "$script_dir/target/release/archivefs-cli" ] || [ -f "$script_dir/target/release/archivefs-gui" ]; then
    src_mode="workspace release build (legacy names)"
    src_dir="$script_dir/target/release"
else
    fail "could not find the emuwiz/emuwiz-cli binaries (or the legacy archivefs-cli/archivefs-gui) next to this script ($script_dir) or under target/release/. If you are in a workspace checkout, run 'cargo build --workspace --release' first."
fi

# Resolve the CLI and GUI sources, accepting either the new or the legacy
# name so old bundles install unchanged.
src_cli=""
for candidate in emuwiz-cli archivefs-cli; do
    if [ -f "$src_dir/$candidate" ]; then
        src_cli="$src_dir/$candidate"
        break
    fi
done
src_gui=""
for candidate in emuwiz archivefs-gui; do
    if [ -f "$src_dir/$candidate" ]; then
        src_gui="$src_dir/$candidate"
        break
    fi
done
missing=""
[ -n "$src_cli" ] || missing="$missing emuwiz-cli/archivefs-cli"
[ -n "$src_gui" ] || missing="$missing emuwiz/archivefs-gui"
if [ -n "$missing" ]; then
    fail "missing required binaries in $src_dir:$missing"
fi

desktop_template="$script_dir/assets/linux/$desktop_id.desktop.in"
branding_dir="$script_dir/assets/branding"
[ -f "$desktop_template" ] || fail "desktop entry template is missing: $desktop_template"
for size in 32 64 128 256 512; do
    [ -f "$branding_dir/emuwiz-logo-$size.png" ] || \
        fail "approved application icon is missing: $branding_dir/emuwiz-logo-$size.png"
done

mkdir -p -- "$bin_dir"
bin_dir=$(CDPATH= cd -- "$bin_dir" && pwd -P) || fail "could not resolve install prefix: $bin_dir"
cp -f -- "$src_cli" "$bin_dir/emuwiz-cli"
chmod +x -- "$bin_dir/emuwiz-cli"
cp -f -- "$src_gui" "$bin_dir/emuwiz"
chmod +x -- "$bin_dir/emuwiz"
# Legacy compatibility aliases: scripts and muscle memory that call
# `archivefs-cli` / `archivefs-gui` keep working.
ln -sf -- emuwiz-cli "$bin_dir/archivefs-cli"
ln -sf -- emuwiz "$bin_dir/emuwiz-gui"
ln -sf -- emuwiz "$bin_dir/archivefs-gui"
printf 'Installed emuwiz-cli and emuwiz to %s (source: %s)\n' "$bin_dir" "$src_mode"
printf 'Aliases emuwiz-gui, archivefs-cli and archivefs-gui still work.\n'

# Desktop Entry Exec values have their own quoting rules. Reject line breaks,
# encode literal percent signs so they cannot become field codes, and escape
# the four characters that are special inside a double-quoted argument.
case "$bin_dir/emuwiz" in
    *'
'*) fail "the install prefix cannot contain a line break" ;;
esac
desktop_exec=$(printf '%s' "$bin_dir/emuwiz" | sed \
    -e 's/\\/\\\\/g' \
    -e 's/"/\\"/g' \
    -e 's/`/\\`/g' \
    -e 's/\$/\\$/g' \
    -e 's/%/%%/g')
desktop_exec="\"$desktop_exec\""

mkdir -p -- "$data_home/applications"
desktop_tmp="$data_home/applications/.$desktop_id.$$.desktop"
while IFS= read -r line || [ -n "$line" ]; do
    if [ "$line" = 'Exec=@EMUWIZ_EXEC@' ]; then
        printf 'Exec=%s\n' "$desktop_exec"
    else
        printf '%s\n' "$line"
    fi
done <"$desktop_template" >"$desktop_tmp"
chmod 0644 "$desktop_tmp"
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop_tmp" || {
        rm -f -- "$desktop_tmp"
        fail "rendered desktop entry failed desktop-file-validate"
    }
fi
mv -f -- "$desktop_tmp" "$desktop_file"

for size in 32 64 128 256 512; do
    icon_dir="$data_home/icons/hicolor/${size}x${size}/apps"
    mkdir -p -- "$icon_dir"
    icon_tmp="$icon_dir/.$desktop_id.$$.png"
    cp -- "$branding_dir/emuwiz-logo-$size.png" "$icon_tmp"
    chmod 0644 "$icon_tmp"
    mv -f -- "$icon_tmp" "$icon_dir/$desktop_id.png"
done
printf 'Installed the EmuWiz desktop launcher and application icons below %s.\n' "$data_home"

mkdir -p -- "$config_dir"
wrote_new_config=0
if [ -e "$config_file" ]; then
    printf 'Existing config found at %s - leaving it untouched.\n' "$config_file"
else
    src_config_example="$script_dir/config.toml.example"
    if [ -f "$src_config_example" ]; then
        cp -- "$src_config_example" "$config_file"
        wrote_new_config=1
        printf 'Wrote a starter config to %s - edit mount_root before running doctor.\n' "$config_file"
        printf 'No source folders are configured yet; that is fine to run with.\n'
    else
        printf '%s: warning: config.toml.example not found next to this script; no starter config was written.\n' "$program_name" >&2
        printf 'Create %s yourself before running emuwiz-cli.\n' "$config_file" >&2
    fi
fi

# Optional first source folder - only offered right after writing a brand
# new config (never for an existing one, which is left untouched above),
# and only when this script has an interactive terminal to prompt on
# (never for a piped/non-interactive install, e.g. `curl ... | sh`).
# Deliberately does not validate the path itself in shell: it is handed
# straight to the just-installed `emuwiz-cli source add`, the exact
# same validated, atomically-persisting function the GUI's Sources page
# and every other source-management entry point uses - no second,
# shell-side implementation of that validation. Skipping (blank input) is
# always safe; more sources can be added the same way, or from the
# Sources page in the GUI, at any time after this script exits.
if [ "$wrote_new_config" -eq 1 ] && [ -t 0 ] && [ -r /dev/tty ]; then
    printf '\n'
    printf 'Add your first archive source folder now? Leave blank to skip and add one\n'
    printf 'later (Sources page in the GUI, or: emuwiz-cli source add PATH).\n'
    printf 'Source folder path: '
    # shellcheck disable=SC2039 # `read -r` is supported by every /bin/sh this script targets.
    if IFS= read -r first_source_path < /dev/tty && [ -n "$first_source_path" ]; then
        if "$bin_dir/emuwiz-cli" source add "$first_source_path"; then
            printf 'Added. Scan it from the Sources page, or: emuwiz-cli source scan "%s"\n' "$first_source_path"
        else
            printf 'Could not add that source folder; you can retry later from the Sources page or: emuwiz-cli source add PATH\n' >&2
        fi
    else
        printf 'Skipped - add one later from the Sources page, or: emuwiz-cli source add PATH\n'
    fi
fi

if command -v ratarmount >/dev/null 2>&1; then
    printf 'ratarmount found: %s\n' "$(command -v ratarmount)"
else
    printf '\n' >&2
    printf 'WARNING: ratarmount was not found on PATH.\n' >&2
    printf 'EmuWiz uses ratarmount to mount archives read-only, and mounting\n' >&2
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
printf '  1. Edit %s (mount_root). Source folders can be managed entirely\n' "$config_file"
printf '     from the app from now on - see the Sources page in the GUI, or\n'
printf '     emuwiz-cli sources / source add / source scan.\n'
if [ "$path_has_bin_dir" -eq 1 ]; then
    printf '  2. Run: emuwiz-cli doctor\n'
    printf '  3. Run: emuwiz-cli config-check\n'
    printf '  4. Launch the GUI: emuwiz\n'
else
    printf '  2. %s is not on your PATH yet. Add it yourself (this script\n' "$bin_dir"
    printf '     will not edit your shell startup files), for example by adding\n'
    printf '     this line to ~/.bashrc or ~/.zshrc:\n'
    printf '       export PATH="%s:$PATH"\n' "$bin_dir"
    printf '     Until then, use the full path:\n'
    printf '  3. Run: %s/emuwiz-cli doctor\n' "$bin_dir"
    printf '  4. Run: %s/emuwiz-cli config-check\n' "$bin_dir"
    printf '  5. Launch the GUI: %s/emuwiz\n' "$bin_dir"
fi

printf '\n'
printf 'To uninstall later: %s/install.sh --uninstall' "$script_dir"
if [ "$bin_dir" != "$default_bin_dir" ]; then
    printf ' --prefix %s' "$bin_dir"
fi
printf '\n'
