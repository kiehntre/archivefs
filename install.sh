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
#
# OWNERSHIP TRACKING
#
# Every path this script writes (binaries, legacy aliases, the desktop
# entry, hicolor icons) is recorded in a small manifest under
# $XDG_DATA_HOME/emuwiz-installer/manifest after a successful install. On
# every later run - reinstall or uninstall - that manifest is what proves a
# given path is still the exact thing this installer put there, rather than
# trusting the filename alone. A path occupied by something else (a foreign
# file, a foreign symlink, a directory) is never silently overwritten or
# removed; it is left alone with a clear warning. See the "OWNERSHIP MODEL"
# comment block below for the full design.
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

# --------------------------------------------------------------------------
# OWNERSHIP MODEL
#
# The manifest lives at $data_home/emuwiz-installer/manifest (never inside
# $data_home/emuwiz, which is the *application's* data directory, kept
# strictly separate so uninstall never has to reach into user data to clean
# up its own bookkeeping). It is a flat, line-oriented, non-executable text
# file - never sourced or eval'd as shell - with a fixed three-line header
# (schema_version, bin_dir, data_home) followed by one line per tracked
# path, keyed by a fixed, closed set of slot names:
#
#   bin-emuwiz-cli, bin-emuwiz                              (regular files)
#   alias-archivefs-cli, alias-emuwiz-gui, alias-archivefs-gui  (symlinks)
#   desktop                                                 (regular file)
#   icon-32, icon-64, icon-128, icon-256, icon-512          (regular files)
#
# Each slot line is "<slot> <kind> <fingerprint>": kind is "file" or
# "symlink". A symlink's fingerprint is its literal target string, which
# this installer only ever sets to one of two fixed values.
#
# A file's fingerprint is its mtime, deliberately *set* by this installer
# (touch -d) to one fixed, this-run-chosen epoch-seconds value shared by
# every file this run installs, rather than read passively from whatever
# the filesystem already assigned. This is chosen over content (a binary's
# bytes legitimately differ release to release, so there is no byte pattern
# to compare) and over inode number, which was tried first and rejected
# after empirical testing on plain ext4 showed a freed inode gets reused by
# the very next allocation with no attacker involved at all - an ordinary
# `rm` immediately followed by a `>` redirect at the same path routinely
# lands back on the identical inode number, which would have made "the
# inode still matches" a false positive for exactly the "something else
# replaced this file" case the whole feature exists to catch. An
# EmuWiz-chosen wall-clock second is not a resource the filesystem
# allocates or reuses, so it does not share that failure mode: a foreign
# write naturally gets "now" as its mtime, and matching our specific past
# install second by chance needs either a coincidence measured in years, or
# a same-account process deliberately targeting this exact scheme - outside
# this installer's threat model (no sudo, current user only; see the
# --replace-foreign backup path for how a knowing user overrides its
# judgement on purpose).
#
# Critically: install.sh NEVER reads a path out of the manifest. Every path
# it might create, verify or remove is always the one it independently
# computes right now from $bin_dir/$data_home/$desktop_id - the same way it
# always has. The manifest is consulted only as a slot-name-keyed lookup
# table of "kind + fingerprint", strictly validated against the shapes above
# on every read; anything that doesn't parse as one of those exact shapes is
# treated as if that slot simply had no record at all. A corrupted,
# truncated or hand-edited manifest can therefore never cause install.sh to
# touch a path other than the fixed set it was already going to consider -
# at worst it makes an owned path look unowned, which only ever means "ask
# again" (fail safe), never "delete something else instead".
#
# No manifest, or a record for a bin_dir/data_home this run doesn't match:
# every slot falls back to a per-asset-type recognition rule instead of
# blind trust - and the rule is only ever "recognise it" where that is
# actually provable, never "assume it because the name matches":
#   - binaries (bin-emuwiz-cli, bin-emuwiz): no recognition rule at all.
#     Binary *content* legitimately differs release to release, so there is
#     no byte pattern - or anything else - that safely proves an existing
#     file is this installer's own without a manifest entry to check
#     against. An unrecorded binary path is always foreign. The practical
#     effect: the first run of this installer version against an install
#     that predates it treats its own existing binaries as foreign and asks
#     for --replace-foreign once; every run after that has a manifest and
#     needs nothing special. A one-time migration step, not a permanent
#     regression - and the only honest answer once "adopt any executable
#     file at this name" is off the table as unprovable.
#   - aliases (alias-*): an existing symlink whose target is exactly the
#     one value this installer would itself write ("emuwiz-cli" or
#     "emuwiz") is adopted. This is close to proof, not a guess - a
#     coincidentally-matching relative symlink target at this exact path is
#     implausible, and unlike a binary's bytes, this target string is fixed
#     content this installer has always written verbatim.
#   - desktop entry: adopted only if it is byte-identical to what this run
#     would render, ignoring the "Exec=" line specifically (which
#     legitimately varies with --prefix across installs). Real proof, not a
#     heuristic.
#   - icons: adopted only if byte-identical to the exact approved source
#     asset for that size. Real proof.
# Anything that fails its recognition rule - including every case a
# directory or a foreign symlink occupies the path - is foreign: never
# overwritten or removed without the caller opting in (--replace-foreign
# for install; uninstall never overwrites/removes a foreign path at all).
#
# The manifest is rewritten from scratch, in full, only after every asset
# this run actually installed has succeeded, via the same mktemp-then-mv
# atomic pattern already used for the desktop entry and icons. A run that
# fails partway through therefore never writes a manifest claiming
# ownership of anything it did not finish installing; whatever it did
# manage to write before failing is picked up correctly on the next
# (successful) run by the very same rules above - including, for a binary
# that got copied but not recorded, being treated as foreign and requiring
# --replace-foreign, exactly like any other unrecorded binary.
# --------------------------------------------------------------------------
manifest_schema_version=1
manifest_dir="$data_home/emuwiz-installer"
manifest_file="$manifest_dir/manifest"
manifest_loaded=0
replace_foreign=0

usage() {
    cat <<EOF
Usage: $program_name [--prefix PATH] [--replace-foreign] [--uninstall] [--help]

Install EmuWiz (emuwiz-cli, emuwiz) for the current user.
No sudo is used and no shell startup files are modified.

Options:
  --prefix PATH      Install the binaries into PATH instead of the default
                      ($default_bin_dir). The directory is created if needed.
                      PATH should normally be an absolute path.
  --replace-foreign   Install only: when a binary, alias, desktop entry or
                      icon destination is occupied by something this
                      installer did not put there, move it aside to
                      "<path>.foreign-backup.<pid>" and install in its
                      place. Without this flag such a path is left
                      untouched and a warning is printed; the rest of the
                      install still proceeds.
  --uninstall         Remove EmuWiz binaries, its desktop entry and application
                      icons - but only the ones still provably EmuWiz-owned.
                      A path that was replaced by something else since it was
                      installed is left alone, with a warning. Your config at
                      $config_file is never touched.
  --help              Show this help and exit.

Without --uninstall, this script:
  1. Detects whether it is running from an extracted release bundle or a
     workspace checkout with release binaries already built, and fails
     with a clear message if neither is found.
  2. Copies emuwiz-cli and emuwiz into the install directory
     and makes sure they are executable - unless that destination is
     occupied by something foreign (see --replace-foreign above).
  3. Installs one EmuWiz desktop launcher and its approved application icons
     below $data_home, subject to the same foreign-path protection.
  4. Records what it just installed in an ownership manifest under
     $manifest_dir, so a later reinstall or uninstall can tell EmuWiz-owned
     paths apart from unrelated ones with the same name.
  5. Creates $config_dir if it does not exist.
  6. Copies config.toml.example to $config_file, but only if that file
     does not already exist. An existing config is never overwritten.
  7. If a new config was just written and this script is running
     interactively, optionally prompts for one archive source folder to
     add via 'emuwiz-cli source add'. Leave blank to skip - source
     folders are never required at install time, and more can always be
     added later from the Sources page in the GUI or the CLI. Never
     offered for an existing config, and never offered for a
     non-interactive install.
  8. Checks whether ratarmount is on PATH and prints installation
     guidance if it is not (EmuWiz uses it to mount archives).
EOF
}

fail() {
    printf '%s: %s\n' "$program_name" "$*" >&2
    exit 1
}

warn() {
    printf '%s: warning: %s\n' "$program_name" "$*" >&2
}

# path_kind PATH - prints "symlink", "dir", "file" or "absent". Always
# checks -L first so a symlink is never misreported as whatever it points
# at (a symlink to a directory is "symlink", not "dir"; a dangling symlink
# is "symlink", not "absent").
path_kind() {
    if [ -L "$1" ]; then
        printf 'symlink\n'
    elif [ -d "$1" ]; then
        printf 'dir\n'
    elif [ -e "$1" ]; then
        printf 'file\n'
    else
        printf 'absent\n'
    fi
}

# install_epoch - the wall-clock second, captured once, that this run
# stamps onto every file it installs (see stamp_fingerprint_mtime). Every
# slot installed in the same run shares this one value, so a single
# manifest write covers all of them consistently.
install_epoch=$(date +%s) || fail "could not read the current time"

# stamp_fingerprint_mtime PATH - sets PATH's mtime to install_epoch. Must
# be called after PATH's content is fully written (cp -f, mv, ln -sf's
# implicit lstat is unaffected since this is only used for kind=file
# slots), and before file_fingerprint reads it back for the manifest.
stamp_fingerprint_mtime() {
    touch -d "@$install_epoch" -- "$1" 2>/dev/null && return 0
    # BSD/busybox touch has no `-d @epoch`; reformat via date and use -t.
    stamp=$(date -d "@$install_epoch" +%Y%m%d%H%M.%S 2>/dev/null) || return 1
    touch -t "$stamp" -- "$1"
}

# file_fingerprint PATH - prints PATH's mtime as whole seconds since the
# epoch, for a regular file. Tries GNU stat's format first, then the
# BSD/busybox one. Prints nothing and returns non-zero if neither works.
file_fingerprint() {
    stat -c '%Y' -- "$1" 2>/dev/null && return 0
    stat -f '%m' -- "$1" 2>/dev/null && return 0
    return 1
}

# validate_file_fp FP - true if FP is one or more digits (a plausible
# epoch-seconds value) and nothing else. Deliberately whole-string strict:
# a manifest line that fails this is treated as if that slot had no record
# at all.
validate_file_fp() {
    case "$1" in
        ''|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

# validate_symlink_fp FP - true only for the exact, finite set of symlink
# targets this installer ever writes.
validate_symlink_fp() {
    case "$1" in
        emuwiz-cli|emuwiz) return 0 ;;
        *) return 1 ;;
    esac
}

# Manifest slot storage: one pair of plain (non-indirect) variables per
# fixed slot name, populated by load_manifest and read directly by the gate
# functions below via manifest_get_slot's literal case match. No eval
# anywhere in the manifest read/write path.
slot_bin_emuwiz_cli_kind=""; slot_bin_emuwiz_cli_fp=""
slot_bin_emuwiz_kind="";     slot_bin_emuwiz_fp=""
slot_alias_archivefs_cli_kind=""; slot_alias_archivefs_cli_fp=""
slot_alias_emuwiz_gui_kind="";    slot_alias_emuwiz_gui_fp=""
slot_alias_archivefs_gui_kind=""; slot_alias_archivefs_gui_fp=""
slot_desktop_kind=""; slot_desktop_fp=""
slot_icon_32_kind="";  slot_icon_32_fp=""
slot_icon_64_kind="";  slot_icon_64_fp=""
slot_icon_128_kind=""; slot_icon_128_fp=""
slot_icon_256_kind=""; slot_icon_256_fp=""
slot_icon_512_kind=""; slot_icon_512_fp=""

# load_manifest - populates manifest_loaded=1, and the slot_* variables
# above, only from a syntactically valid, current-schema-version manifest
# whose recorded bin_dir/data_home match this invocation's. Any mismatch or
# parse problem leaves manifest_loaded=0 and every slot_* empty - callers
# then use the no-manifest recognition rules for everything, never a
# partially-trusted mix.
load_manifest() {
    manifest_loaded=0
    manifest_version=""
    manifest_saved_bin_dir=""
    manifest_saved_data_home=""

    [ -f "$manifest_file" ] || return 0
    [ ! -L "$manifest_file" ] || return 0

    while IFS=' ' read -r key rest || [ -n "${key:-}" ]; do
        case "$key" in
            schema_version) manifest_version=$rest ;;
            bin_dir) manifest_saved_bin_dir=$rest ;;
            data_home) manifest_saved_data_home=$rest ;;
            bin-emuwiz-cli|bin-emuwiz|desktop|icon-32|icon-64|icon-128|icon-256|icon-512)
                slot_kind_field=""
                slot_fp_field=""
                IFS=' ' read -r slot_kind_field slot_fp_field <<EOF
$rest
EOF
                if [ "$slot_kind_field" = file ] && validate_file_fp "$slot_fp_field"; then
                    case "$key" in
                        bin-emuwiz-cli) slot_bin_emuwiz_cli_kind=file; slot_bin_emuwiz_cli_fp=$slot_fp_field ;;
                        bin-emuwiz) slot_bin_emuwiz_kind=file; slot_bin_emuwiz_fp=$slot_fp_field ;;
                        desktop) slot_desktop_kind=file; slot_desktop_fp=$slot_fp_field ;;
                        icon-32) slot_icon_32_kind=file; slot_icon_32_fp=$slot_fp_field ;;
                        icon-64) slot_icon_64_kind=file; slot_icon_64_fp=$slot_fp_field ;;
                        icon-128) slot_icon_128_kind=file; slot_icon_128_fp=$slot_fp_field ;;
                        icon-256) slot_icon_256_kind=file; slot_icon_256_fp=$slot_fp_field ;;
                        icon-512) slot_icon_512_kind=file; slot_icon_512_fp=$slot_fp_field ;;
                    esac
                fi
                ;;
            alias-archivefs-cli|alias-emuwiz-gui|alias-archivefs-gui)
                slot_kind_field=""
                slot_fp_field=""
                IFS=' ' read -r slot_kind_field slot_fp_field <<EOF
$rest
EOF
                if [ "$slot_kind_field" = symlink ] && validate_symlink_fp "$slot_fp_field"; then
                    case "$key" in
                        alias-archivefs-cli) slot_alias_archivefs_cli_kind=symlink; slot_alias_archivefs_cli_fp=$slot_fp_field ;;
                        alias-emuwiz-gui) slot_alias_emuwiz_gui_kind=symlink; slot_alias_emuwiz_gui_fp=$slot_fp_field ;;
                        alias-archivefs-gui) slot_alias_archivefs_gui_kind=symlink; slot_alias_archivefs_gui_fp=$slot_fp_field ;;
                    esac
                fi
                ;;
            *) : ;;
        esac
    done <"$manifest_file"

    [ "$manifest_version" = "$manifest_schema_version" ] || return 0
    [ -n "$manifest_saved_bin_dir" ] || return 0
    [ "$manifest_saved_bin_dir" = "$bin_dir" ] || return 0
    [ -n "$manifest_saved_data_home" ] || return 0
    [ "$manifest_saved_data_home" = "$data_home" ] || return 0
    manifest_loaded=1
}

# manifest_get_slot SLOT - sets slot_kind/slot_fp to the manifest's record
# for SLOT (both empty if manifest_loaded=0 or SLOT has no record). Maps
# SLOT to the corresponding slot_<name>_kind/_fp pair via a literal case
# match only - never eval, never indirection driven by file content.
manifest_get_slot() {
    slot_kind=""
    slot_fp=""
    [ "$manifest_loaded" -eq 1 ] || return 0
    case "$1" in
        bin-emuwiz-cli) slot_kind=$slot_bin_emuwiz_cli_kind; slot_fp=$slot_bin_emuwiz_cli_fp ;;
        bin-emuwiz) slot_kind=$slot_bin_emuwiz_kind; slot_fp=$slot_bin_emuwiz_fp ;;
        alias-archivefs-cli) slot_kind=$slot_alias_archivefs_cli_kind; slot_fp=$slot_alias_archivefs_cli_fp ;;
        alias-emuwiz-gui) slot_kind=$slot_alias_emuwiz_gui_kind; slot_fp=$slot_alias_emuwiz_gui_fp ;;
        alias-archivefs-gui) slot_kind=$slot_alias_archivefs_gui_kind; slot_fp=$slot_alias_archivefs_gui_fp ;;
        desktop) slot_kind=$slot_desktop_kind; slot_fp=$slot_desktop_fp ;;
        icon-32) slot_kind=$slot_icon_32_kind; slot_fp=$slot_icon_32_fp ;;
        icon-64) slot_kind=$slot_icon_64_kind; slot_fp=$slot_icon_64_fp ;;
        icon-128) slot_kind=$slot_icon_128_kind; slot_fp=$slot_icon_128_fp ;;
        icon-256) slot_kind=$slot_icon_256_kind; slot_fp=$slot_icon_256_fp ;;
        icon-512) slot_kind=$slot_icon_512_kind; slot_fp=$slot_icon_512_fp ;;
    esac
}

# desktop_matches_ignoring_exec FILE1 FILE2 - true if FILE1 and FILE2 are
# line-for-line identical except that any line starting "Exec=" in one is
# accepted against any line starting "Exec=" in the other, unconditionally.
# Used only to recognise a pre-manifest desktop entry this installer wrote
# for a *different* --prefix, without requiring the Exec value itself to
# match.
desktop_matches_ignoring_exec() {
    a=$1
    b=$2
    [ -f "$a" ] && [ ! -L "$a" ] || return 1
    [ -f "$b" ] && [ ! -L "$b" ] || return 1
    exec 3<"$a" || return 1
    exec 4<"$b" || { exec 3<&-; return 1; }
    matched=0
    while :; do
        line_a=""; have_a=0
        if IFS= read -r line_a <&3; then have_a=1
        elif [ -n "$line_a" ]; then have_a=1
        fi
        line_b=""; have_b=0
        if IFS= read -r line_b <&4; then have_b=1
        elif [ -n "$line_b" ]; then have_b=1
        fi
        if [ "$have_a" -eq 0 ] && [ "$have_b" -eq 0 ]; then
            matched=1
            break
        fi
        if [ "$have_a" -eq 0 ] || [ "$have_b" -eq 0 ]; then
            break
        fi
        case "$line_a" in
            Exec=*)
                case "$line_b" in
                    Exec=*) ;;
                    *) break ;;
                esac
                ;;
            *)
                [ "$line_a" = "$line_b" ] || break
                ;;
        esac
    done
    exec 3<&- 4<&-
    [ "$matched" -eq 1 ]
}

# gate_binary SLOT PATH - prints absent/owned/foreign for a regular EmuWiz
# binary destination. There is no pre-manifest recognition rule here at
# all, unlike aliases/desktop/icons below: a binary's content legitimately
# differs release to release, so there is no byte pattern - or any other
# signal - that safely proves a pre-existing file is this installer's own
# without a manifest record to check against. An unrecorded path is always
# foreign. In practice this means the very first run of this installer
# version, against an install that predates it, treats its own existing
# binaries as foreign once and asks for --replace-foreign - a one-time
# migration step, not a permanent regression, and the honest answer given
# the alternative is silently trusting a filename.
gate_binary() {
    slot=$1
    path=$2
    case $(path_kind "$path") in
        absent) printf 'absent\n'; return 0 ;;
        symlink|dir) printf 'foreign\n'; return 0 ;;
    esac
    manifest_get_slot "$slot"
    if [ "$slot_kind" = file ]; then
        current_fp=$(file_fingerprint "$path") || current_fp=""
        if [ -n "$current_fp" ] && [ "$current_fp" = "$slot_fp" ]; then
            printf 'owned\n'
            return 0
        fi
    fi
    printf 'foreign\n'
}

# gate_alias SLOT PATH EXPECTED_TARGET - prints absent/owned/foreign for a
# legacy symlink alias destination.
gate_alias() {
    slot=$1
    path=$2
    expected_target=$3
    case $(path_kind "$path") in
        absent) printf 'absent\n'; return 0 ;;
        dir) printf 'foreign\n'; return 0 ;;
        file) printf 'foreign\n'; return 0 ;;
    esac
    manifest_get_slot "$slot"
    if [ -n "$slot_kind" ]; then
        if [ "$slot_kind" = symlink ]; then
            current_target=$(readlink -- "$path" 2>/dev/null) || current_target=""
            if [ -n "$current_target" ] && [ "$current_target" = "$slot_fp" ]; then
                printf 'owned\n'
                return 0
            fi
        fi
        printf 'foreign\n'
        return 0
    fi
    current_target=$(readlink -- "$path" 2>/dev/null) || current_target=""
    if [ -n "$current_target" ] && [ "$current_target" = "$expected_target" ]; then
        printf 'owned\n'
    else
        printf 'foreign\n'
    fi
}

# gate_content SLOT PATH REFERENCE NORMALIZE - prints absent/owned/foreign
# for the desktop entry (NORMALIZE=exec, REFERENCE is the freshly-rendered
# candidate to compare against ignoring Exec=) or an icon (NORMALIZE=,
# REFERENCE is the exact approved source PNG for that size). If REFERENCE
# itself is unavailable (e.g. uninstalling without the original release
# bundle present), the pre-manifest recognition rule cannot run and an
# unrecorded path is conservatively reported foreign rather than skipped.
gate_content() {
    slot=$1
    path=$2
    reference=$3
    normalize=$4
    case $(path_kind "$path") in
        absent) printf 'absent\n'; return 0 ;;
        symlink|dir) printf 'foreign\n'; return 0 ;;
    esac
    manifest_get_slot "$slot"
    if [ -n "$slot_kind" ]; then
        if [ "$slot_kind" = file ]; then
            current_fp=$(file_fingerprint "$path") || current_fp=""
            if [ -n "$current_fp" ] && [ "$current_fp" = "$slot_fp" ]; then
                printf 'owned\n'
                return 0
            fi
        fi
        printf 'foreign\n'
        return 0
    fi
    if [ -z "$reference" ] || [ ! -f "$reference" ]; then
        printf 'foreign\n'
        return 0
    fi
    if [ "$normalize" = exec ]; then
        if desktop_matches_ignoring_exec "$path" "$reference"; then
            printf 'owned\n'
        else
            printf 'foreign\n'
        fi
    else
        if cmp -s -- "$path" "$reference"; then
            printf 'owned\n'
        else
            printf 'foreign\n'
        fi
    fi
}

# resolve_foreign_collision PATH - called only when a gate function reports
# "foreign". Without --replace-foreign: warns and returns 1 (caller must
# leave PATH alone). With --replace-foreign: moves PATH aside to
# "PATH.foreign-backup.$$" (never overwriting a previous backup blindly -
# the pid makes each run's backup name distinct) and returns 0 so the
# caller proceeds to install fresh.
resolve_foreign_collision() {
    path=$1
    if [ "$replace_foreign" -ne 1 ]; then
        warn "leaving foreign path untouched (not EmuWiz-owned): $path"
        warn "  re-run with --replace-foreign to move it aside and install here"
        skipped_any=1
        return 1
    fi
    backup="$path.foreign-backup.$$"
    mv -f -- "$path" "$backup" || fail "could not move foreign path aside: $path"
    warn "moved foreign path aside before installing: $path -> $backup"
    return 0
}

# record_slot SLOT KIND PATH - appends a manifest line for SLOT to
# $manifest_tmp, fingerprinting PATH fresh right now. Only ever called
# immediately after (re)installing PATH, so the recorded fingerprint always
# reflects exactly what was just written.
record_slot() {
    slot=$1
    kind=$2
    path=$3
    case "$kind" in
        file)
            stamp_fingerprint_mtime "$path" || fail "could not set the install timestamp on: $path"
            fp=$(file_fingerprint "$path") || fail "could not fingerprint installed file: $path"
            ;;
        symlink)
            fp=$(readlink -- "$path") || fail "could not read installed symlink: $path"
            ;;
        *)
            fail "internal error: unknown manifest slot kind: $kind"
            ;;
    esac
    printf '%s %s %s\n' "$slot" "$kind" "$fp" >>"$manifest_tmp"
}

# remove_if_owned SLOT KIND PATH [EXPECTED_OR_REFERENCE] [NORMALIZE] -
# removes PATH during --uninstall only if a gate function reports it
# "owned". Absent paths are silently skipped (nothing to warn about); a
# directory is always left alone with a warning. Foreign paths are left
# alone with a warning and never forced (--replace-foreign has no effect
# during --uninstall - there is nothing to "replace" when only removing).
remove_if_owned() {
    slot=$1
    kind=$2
    path=$3
    extra=${4:-}
    normalize=${5:-}

    case $(path_kind "$path") in
        absent) return 0 ;;
    esac

    case "$kind" in
        symlink) gate=$(gate_alias "$slot" "$path" "$extra") ;;
        file) gate=$(gate_content "$slot" "$path" "$extra" "$normalize") ;;
        binary) gate=$(gate_binary "$slot" "$path") ;;
    esac

    case "$gate" in
        owned)
            rm -f -- "$path" || fail "could not remove $path"
            printf 'Removed %s\n' "$path"
            removed_any=1
            ;;
        *)
            case $(path_kind "$path") in
                dir) warn "expected $slot at $path but found a directory; leaving it in place" ;;
                *) warn "leaving foreign path untouched (not EmuWiz-owned): $path" ;;
            esac
            left_foreign=1
            ;;
    esac
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
        --replace-foreign)
            replace_foreign=1
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

# desktop_template/branding_dir are resolved here (not just inside the
# install path below) because --uninstall's pre-manifest recognition rule
# for the desktop entry and icons needs the same reference content to
# compare against. Best-effort for uninstall: a missing bundle only means
# those two specific pre-manifest checks fall back to "foreign" (see
# gate_content) rather than failing the whole uninstall - a manifest from
# any successful install makes this irrelevant on every later run.
desktop_template="$script_dir/assets/linux/$desktop_id.desktop.in"
branding_dir="$script_dir/assets/branding"

if [ "$do_uninstall" -eq 1 ]; then
    # bin_dir must be an absolute, resolved path for manifest bin_dir
    # comparison to have any chance of matching what install recorded -
    # mirror the same resolution install uses, but tolerate a prefix that
    # does not exist (there may be nothing left to uninstall there at all).
    if [ -d "$bin_dir" ]; then
        bin_dir=$(CDPATH= cd -- "$bin_dir" && pwd -P) || fail "could not resolve install prefix: $bin_dir"
    fi

    load_manifest

    removed_any=0
    left_foreign=0

    remove_if_owned bin-emuwiz-cli binary "$bin_dir/emuwiz-cli"
    remove_if_owned bin-emuwiz binary "$bin_dir/emuwiz"
    remove_if_owned alias-archivefs-cli symlink "$bin_dir/archivefs-cli" emuwiz-cli
    remove_if_owned alias-emuwiz-gui symlink "$bin_dir/emuwiz-gui" emuwiz
    remove_if_owned alias-archivefs-gui symlink "$bin_dir/archivefs-gui" emuwiz

    desktop_reference=""
    if [ -f "$desktop_template" ]; then
        # Render what a legitimate desktop entry for the CURRENT bin_dir
        # would look like, purely so the pre-manifest gate can compare
        # against it (ignoring Exec=, since a manifest match already covers
        # the "same bin_dir as before" case without needing this at all).
        case "$bin_dir/emuwiz" in
            *'
'*) : ;;
            *)
                desktop_reference=$(mktemp -- "${TMPDIR:-/tmp}/.emuwiz-uninstall-ref.XXXXXX") || desktop_reference=""
                if [ -n "$desktop_reference" ]; then
                    desktop_exec_ref=$(printf '%s' "$bin_dir/emuwiz" | sed \
                        -e 's/\\/\\\\\\\\/g' \
                        -e 's/"/\\"/g' \
                        -e 's/`/\\`/g' \
                        -e 's/\$/\\$/g' \
                        -e 's/%/%%/g')
                    desktop_exec_ref="\"$desktop_exec_ref\""
                    while IFS= read -r line || [ -n "$line" ]; do
                        if [ "$line" = 'Exec=@EMUWIZ_EXEC@' ]; then
                            printf 'Exec=%s\n' "$desktop_exec_ref"
                        else
                            printf '%s\n' "$line"
                        fi
                    done <"$desktop_template" >"$desktop_reference" 2>/dev/null || desktop_reference=""
                fi
                ;;
        esac
    fi
    remove_if_owned desktop file "$desktop_file" "$desktop_reference" exec
    [ -z "$desktop_reference" ] || rm -f -- "$desktop_reference"

    for size in 32 64 128 256 512; do
        icon_reference="$branding_dir/emuwiz-logo-$size.png"
        [ -f "$icon_reference" ] || icon_reference=""
        remove_if_owned "icon-$size" file \
            "$data_home/icons/hicolor/${size}x${size}/apps/$desktop_id.png" \
            "$icon_reference"
    done

    if [ "$removed_any" -eq 0 ]; then
        printf 'Nothing to uninstall in %s\n' "$bin_dir"
    fi
    if [ "$left_foreign" -eq 1 ]; then
        printf 'Some paths were left in place because they are not provably EmuWiz-owned; see the warnings above.\n' >&2
    fi
    # Clean up the installer's own bookkeeping only once nothing was left
    # behind - if something was left foreign, keep the manifest around so a
    # future run (or a human) still has the ownership record to work from.
    if [ "$left_foreign" -eq 0 ] && [ -f "$manifest_file" ]; then
        rm -f -- "$manifest_file"
        rmdir -- "$manifest_dir" 2>/dev/null || true
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

[ -f "$desktop_template" ] || fail "desktop entry template is missing: $desktop_template"
for size in 32 64 128 256 512; do
    [ -f "$branding_dir/emuwiz-logo-$size.png" ] || \
        fail "approved application icon is missing: $branding_dir/emuwiz-logo-$size.png"
done

mkdir -p -- "$bin_dir"
bin_dir=$(CDPATH= cd -- "$bin_dir" && pwd -P) || fail "could not resolve install prefix: $bin_dir"

load_manifest

mkdir -p -- "$manifest_dir"
manifest_tmp=$(mktemp -- "$manifest_dir/.manifest.XXXXXX") ||
    fail "could not create a temporary file for the install manifest"
{
    printf '# EmuWiz installer ownership manifest - machine-generated, do not hand-edit.\n'
    printf '# Regenerated in full on every successful install/reinstall.\n'
    printf 'schema_version %s\n' "$manifest_schema_version"
    printf 'bin_dir %s\n' "$bin_dir"
    printf 'data_home %s\n' "$data_home"
} >"$manifest_tmp"

skipped_any=0

# install_binary_slot SLOT SRC DEST - copies SRC to DEST and marks it
# executable, unless DEST is occupied by something foreign (in which case
# it is skipped, or backed up first with --replace-foreign).
install_binary_slot() {
    slot=$1
    src=$2
    dest=$3
    gate=$(gate_binary "$slot" "$dest")
    if [ "$gate" = foreign ]; then
        resolve_foreign_collision "$dest" || return 0
    fi
    cp -f -- "$src" "$dest"
    chmod +x -- "$dest"
    record_slot "$slot" file "$dest"
}

# install_alias_slot SLOT TARGET DEST - creates the symlink DEST -> TARGET,
# unless DEST is occupied by something foreign.
install_alias_slot() {
    slot=$1
    target=$2
    dest=$3
    gate=$(gate_alias "$slot" "$dest" "$target")
    if [ "$gate" = foreign ]; then
        resolve_foreign_collision "$dest" || return 0
    fi
    ln -sf -- "$target" "$dest"
    record_slot "$slot" symlink "$dest"
}

install_binary_slot bin-emuwiz-cli "$src_cli" "$bin_dir/emuwiz-cli"
install_binary_slot bin-emuwiz "$src_gui" "$bin_dir/emuwiz"
# Legacy compatibility aliases: scripts and muscle memory that call
# `archivefs-cli` / `archivefs-gui` keep working.
install_alias_slot alias-archivefs-cli emuwiz-cli "$bin_dir/archivefs-cli"
install_alias_slot alias-emuwiz-gui emuwiz "$bin_dir/emuwiz-gui"
install_alias_slot alias-archivefs-gui emuwiz "$bin_dir/archivefs-gui"
printf 'Installed emuwiz-cli and emuwiz to %s (source: %s)\n' "$bin_dir" "$src_mode"
printf 'Aliases emuwiz-gui, archivefs-cli and archivefs-gui still work.\n'

# Desktop Entry Exec values have their own quoting rules. Reject line breaks,
# encode literal percent signs so they cannot become field codes, and escape
# the four characters that are special inside a double-quoted argument.
#
# Backslash is doubled to *four* backslashes, not two: a Desktop Entry
# string-type value (Exec included) is first run through the format's
# generic backslash-unescape pass (which recognises only \\, \s, \n, \t,
# \r) before the Exec-specific quoting rules below are ever applied to it.
# That generic pass collapses "\\" down to one "\" before an Exec parser
# sees it, so to have *one* backslash survive as the Exec-quoted escape
# sequence "\\" (which itself decodes to one literal backslash), the file
# must contain "\\\\". Two backslashes here would collapse to a single
# stray "\" that desktop-file-validate then folds into whatever character
# follows it, breaking quote tracking for the rest of the value - which is
# exactly what upstream desktop-file-utils' own parser documents in
# src/validate.c above handle_exec_key().
case "$bin_dir/emuwiz" in
    *'
'*) fail "the install prefix cannot contain a line break" ;;
esac
desktop_exec=$(printf '%s' "$bin_dir/emuwiz" | sed \
    -e 's/\\/\\\\\\\\/g' \
    -e 's/"/\\"/g' \
    -e 's/`/\\`/g' \
    -e 's/\$/\\$/g' \
    -e 's/%/%%/g')
desktop_exec="\"$desktop_exec\""

mkdir -p -- "$data_home/applications"
# Render the candidate into a temp file first, before any ownership
# decision, so the "does the existing file match what we'd write" gate and
# the eventual atomic install always compare against and use the exact
# same rendered bytes.
desktop_tmp=$(mktemp -- "$data_home/applications/.$desktop_id.XXXXXX.desktop") ||
    fail "could not create a temporary file for the desktop entry"
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

desktop_gate=$(gate_content desktop "$desktop_file" "$desktop_tmp" exec)
desktop_installed=0
if [ "$desktop_gate" = foreign ]; then
    if resolve_foreign_collision "$desktop_file"; then
        desktop_installed=1
    fi
else
    desktop_installed=1
fi
if [ "$desktop_installed" -eq 1 ]; then
    mv -f -- "$desktop_tmp" "$desktop_file"
    record_slot desktop file "$desktop_file"
    printf 'Installed the EmuWiz desktop launcher below %s.\n' "$data_home"
else
    rm -f -- "$desktop_tmp"
fi

for size in 32 64 128 256 512; do
    icon_dir="$data_home/icons/hicolor/${size}x${size}/apps"
    mkdir -p -- "$icon_dir"
    icon_dest="$icon_dir/$desktop_id.png"
    icon_source="$branding_dir/emuwiz-logo-$size.png"
    icon_gate=$(gate_content "icon-$size" "$icon_dest" "$icon_source" "")
    if [ "$icon_gate" = foreign ]; then
        resolve_foreign_collision "$icon_dest" || continue
    fi
    icon_tmp=$(mktemp -- "$icon_dir/.$desktop_id.XXXXXX.png") ||
        fail "could not create a temporary file for the $size pixel icon"
    cp -- "$icon_source" "$icon_tmp"
    chmod 0644 "$icon_tmp"
    mv -f -- "$icon_tmp" "$icon_dest"
    record_slot "icon-$size" file "$icon_dest"
done
printf 'Installed the EmuWiz application icons below %s.\n' "$data_home"

chmod 0600 "$manifest_tmp"
mv -f -- "$manifest_tmp" "$manifest_file"

if [ "$skipped_any" -eq 1 ]; then
    printf '\n' >&2
    printf 'One or more destinations were left untouched because they are not\n' >&2
    printf 'provably EmuWiz-owned; see the warnings above. Re-run with the\n' >&2
    printf 'flag "--replace-foreign" to move them aside and install there anyway.\n' >&2
fi

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

if [ "$skipped_any" -eq 1 ]; then
    exit 1
fi
