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
# trusting the filename, its timestamp, or its inode alone. A path occupied
# by something else (a foreign file, a foreign symlink, a directory) is
# never silently overwritten or removed; it is left alone with a clear
# warning. See the "OWNERSHIP MODEL" comment block below for the full
# design.
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
# file - never sourced or eval'd as shell - with a fixed header (schema_version,
# bin_dir, data_home, record_count) followed by exactly record_count lines,
# one per tracked path, keyed by a fixed, closed set of slot names, and
# terminated by a literal "end" line:
#
#   bin-emuwiz-cli, bin-emuwiz                              (regular files)
#   alias-archivefs-cli, alias-emuwiz-gui, alias-archivefs-gui  (symlinks)
#   desktop                                                 (regular file)
#   icon-32, icon-64, icon-128, icon-256, icon-512          (regular files)
#
# A file slot line is "<slot> file <sha256-hex> <size-bytes>": the SHA-256
# digest of the installed file's exact byte content is the sole ownership
# proof - size is recorded only as supplemental metadata for diagnostics
# and is never consulted by any ownership decision. A symlink slot line is
# "<slot> symlink <target>": the target is the symlink's literal, raw
# readlink() value, which this installer only ever sets to one of two fixed
# values.
#
# CONTENT, not mtime, not inode. mtime was the original design and was
# rejected after a hostile review reproduced two real failures: `touch -r`
# from a genuinely owned file, or `cp -p`/`touch -d` to the installer's own
# recorded timestamp, let a byte-for-byte DIFFERENT foreign file pass the
# ownership check purely by forging the one signal being trusted. Inode
# number was tried before that and rejected too - empirical testing on
# plain ext4 showed a freed inode gets reused by the very next allocation
# with no attacker involved at all, so "the inode still matches" was
# already a false positive waiting to happen even without a forged
# timestamp. A cryptographic digest of the actual bytes has no equivalent
# forgery surface within this installer's threat model (no sudo, current
# user only, no chosen-prefix-collision attack against SHA-256 itself) -
# forging it requires reproducing the exact content, which is precisely
# what "provably the same file" means.
#
# Critically: install.sh NEVER reads a path out of the manifest. Every path
# it might create, verify or remove is always the one it independently
# computes right now from $bin_dir/$data_home/$desktop_id - the same way it
# always has. The manifest is consulted only as a slot-name-keyed lookup
# table of "kind + digest [+ size]", strictly validated against the shapes
# above on every read; anything that doesn't parse as one of those exact
# shapes invalidates the WHOLE manifest (see the parser rules below), never
# just the one bad line. A corrupted, truncated or hand-edited manifest can
# therefore never cause install.sh to touch a path other than the fixed set
# it was already going to consider - at worst it makes an owned path look
# unowned, which only ever means "ask again" (fail safe), never "delete
# something else instead".
#
# PARSER: fail-closed, not best-effort. A manifest is accepted only if, in
# one pass:
#   - exactly one schema_version line, whose value is the current schema
#   - exactly one bin_dir line, matching this invocation's resolved bin_dir
#   - exactly one data_home line, matching this invocation's data_home
#   - exactly one record_count line, a plain non-negative integer
#   - zero or more slot record lines, each for one of the fixed slot names
#     above, each appearing at most once (a second line for a slot already
#     seen invalidates the whole manifest - never "last one wins")
#   - a slot record's fields are validated strictly: a file slot's digest
#     must be exactly 64 lowercase hex characters and its size a plain
#     digit string; a symlink slot's target must be exactly one of the
#     finite set of targets this installer ever writes. Anything else
#     invalidates the whole manifest.
#   - no line of any other shape (an unrecognised key, a duplicate header,
#     anything appearing after the end marker) - the whole manifest is
#     invalidated, not just that line
#   - a single literal "end" line, exactly once, as the last line - nothing
#     may follow it, and a manifest is never accepted without one
#   - the number of slot record lines actually present exactly equals the
#     declared record_count
# Both record_count AND the "end" marker are required (belt and braces):
# either one alone already rules out "valid header followed by truncation"
# passing as complete, since a truncated file can supply at most one of
# "the declared count matches" or "the file has a terminator" but never a
# consistent forgery of both without simply being the real, untruncated
# file. A manifest that fails ANY of these checks is treated exactly like
# no manifest existing at all - every slot then falls back to the
# per-asset-type recognition rules below.
#
# Intentionally partial manifests ARE part of the design: a run that left
# some destinations foreign (no --replace-foreign) still writes a manifest
# recording only the slots it actually installed - record_count reflects
# that smaller number, not 11. A slot with no record in an otherwise valid,
# accepted manifest simply falls back to the same per-asset-type
# recognition rule as "no manifest at all", scoped to just that slot.
#
# No manifest, or a record for a bin_dir/data_home this run doesn't match,
# or no record for a given slot: that slot falls back to a per-asset-type
# recognition rule instead of blind trust - and the rule is only ever
# "recognise it" where that is actually provable, never "assume it because
# the name matches":
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
# UPGRADE ORDERING: a legitimate reinstall over a newer release's binaries
# gates on the digest currently sitting at the destination - the OLD
# installed content - against the OLD recorded manifest digest, strictly
# BEFORE that destination is overwritten with the new bundled asset. Only
# once that gate passes does the copy happen, and only after the copy
# succeeds is the NEW digest computed and recorded. The new bundled asset's
# own digest never enters the ownership decision for what it is about to
# replace - comparing "what's already installed" against "what we're about
# to install" would make every version bump look foreign (the bytes always
# differ) and would also make an unrelated foreign file that happens to
# match some OTHER version's digest look owned. Both directions are wrong;
# only "does the CURRENT installed content match the LAST thing we
# ourselves recorded" is the right question.
#
# The manifest is rewritten from scratch, in full, only after every asset
# this run actually installed has succeeded, via the same mktemp-then-mv
# atomic pattern already used for the desktop entry and icons - and it is
# written into the SAME directory it replaces, so the rename is a single
# same-filesystem operation, never a cross-directory or cross-filesystem
# copy. A run that fails partway through therefore never writes a manifest
# claiming ownership of anything it did not finish installing; whatever it
# did manage to write before failing is picked up correctly on the next
# (successful) run by the very same rules above - including, for a binary
# that got copied but not recorded, being treated as foreign and requiring
# --replace-foreign, exactly like any other unrecorded binary.
#
# MANIFEST-DIRECTORY SAFETY: $data_home itself - wherever XDG_DATA_HOME
# resolves to, symlinked or not - is trusted exactly as much as it already
# was for every other asset this installer writes there (the desktop entry,
# the icons); a symlinked XDG_DATA_HOME pointing at a different disk is a
# completely ordinary, legitimate setup and nothing about ownership
# tracking treats it with any more suspicion than the rest of this script
# always has. The boundary that IS enforced is narrower and specific to
# this feature: the final "emuwiz-installer" path component, which this
# installer alone creates and fully owns, must be a real directory - never
# a symlink, and never anything else occupying that exact name. If it does
# not exist yet, install creates it fresh as a real directory, mode 0700.
# If something already occupies that name and is not a real directory,
# install refuses to proceed at all (there is nowhere left it could
# trustworthily record ownership of anything this run installs);
# uninstall, which never creates anything, simply treats it exactly like
# "no manifest" and never touches that path. Symmetrically, the manifest
# FILE itself is only ever replaced once it has been validated as belonging
# to this installation - which in practice means it loaded successfully
# under load_manifest's own parser rules earlier in the same run, or did
# not exist at all. A pre-existing path there that did NOT load - a
# symlink, a directory, or simply a regular file whose content is not one
# of our own manifests - is treated exactly like any other foreign
# collision in this script: left untouched with a warning by default
# (asset installs this run still proceed and are simply not recorded until
# that is resolved), or moved aside into a fresh backup - never silently
# written through or over - with --replace-foreign.
# --------------------------------------------------------------------------
manifest_schema_version=2
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
  --replace-foreign   Install only: when a binary, alias, desktop entry,
                      icon, or the ownership manifest itself is occupied by
                      something this installer did not put there, move it
                      aside into a freshly and securely created backup
                      directory next to it and install in its place. The
                      exact backup location is printed. Without this flag
                      such a path is left untouched and a warning is
                      printed; the rest of the install still proceeds.
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
  4. Records what it just installed in a SHA-256-backed ownership manifest
     under $manifest_dir, so a later reinstall or uninstall can tell
     EmuWiz-owned paths apart from unrelated ones with the same name -
     never by filename, timestamp or inode alone.
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

# manifest_dir_state - "dir" (safe to read from and, on install, write
# into), "absent" (nothing there yet - install may create it fresh) or
# "unsafe" (a symlink, a regular file, or anything else occupying the
# installer bookkeeping directory's path - never traversed, read through,
# or created-through). Computed once, before manifest_dir is used for
# anything at all.
manifest_dir_state() {
    case $(path_kind "$manifest_dir") in
        dir) printf 'dir\n' ;;
        absent) printf 'absent\n' ;;
        *) printf 'unsafe\n' ;;
    esac
}
manifest_dir_kind=$(manifest_dir_state)

# ensure_manifest_dir - called only from the install path, only after
# load_manifest has already captured whatever the PRE-run state was. Creates
# the bookkeeping directory fresh (mode 0700) if nothing occupies that name
# yet; tightens permissions on an existing real directory; refuses outright
# (fails the whole install - there is nowhere left it could trustworthily
# record anything) if something unsafe occupies that name. Deliberately
# plain mkdir, never mkdir -p: -p treats a name that already resolves to
# something (including through a symlink) as success, which is exactly the
# ambiguity this function exists to close.
ensure_manifest_dir() {
    case "$manifest_dir_kind" in
        dir)
            chmod 0700 -- "$manifest_dir" 2>/dev/null || true
            ;;
        absent)
            # $data_home itself is the same pre-existing, already-trusted
            # boundary every other asset this script writes relies on
            # (mkdir -p here is fine - it is not the hardened boundary,
            # manifest_dir's own final component below is). Only that last
            # component is ever created via a plain, non-p mkdir.
            mkdir -p -- "$data_home" ||
                fail "could not create the data directory: $data_home"
            mkdir -m 0700 -- "$manifest_dir" ||
                fail "could not create the installer bookkeeping directory: $manifest_dir"
            case $(path_kind "$manifest_dir") in
                dir) : ;;
                *) fail "installer bookkeeping directory did not come up safe after creation: $manifest_dir" ;;
            esac
            manifest_dir_kind=dir
            ;;
        unsafe)
            fail "installer bookkeeping directory exists but is not a plain directory (found a $(path_kind "$manifest_dir")): $manifest_dir - remove or rename it by hand, then re-run"
            ;;
    esac
}

# file_digest PATH - prints the SHA-256 digest of PATH's exact byte content
# as 64 lowercase hex characters. Tries sha256sum, then shasum -a 256, then
# openssl dgst -sha256 -r (all three emit "<hex>  <name>"-shaped output, so
# only the first field is ever taken). Prints nothing and returns non-zero
# if none of those tools is available or the read fails.
file_digest() {
    path=$1
    line=""
    if command -v sha256sum >/dev/null 2>&1; then
        line=$(sha256sum -- "$path" 2>/dev/null) || return 1
    elif command -v shasum >/dev/null 2>&1; then
        line=$(shasum -a 256 -- "$path" 2>/dev/null) || return 1
    elif command -v openssl >/dev/null 2>&1; then
        line=$(openssl dgst -sha256 -r -- "$path" 2>/dev/null) || return 1
    else
        return 1
    fi
    [ -n "$line" ] || return 1
    set -- $line
    printf '%s\n' "$1"
}

# file_size PATH - prints PATH's size in bytes. Tries GNU stat's format
# first, then the BSD/busybox one. Supplemental metadata only - never
# consulted by any ownership decision.
file_size() {
    stat -c '%s' -- "$1" 2>/dev/null && return 0
    stat -f '%z' -- "$1" 2>/dev/null && return 0
    return 1
}

# validate_file_digest FP - true only for a string of exactly 64 lowercase
# hex characters and nothing else.
validate_file_digest() {
    fp=$1
    [ "${#fp}" -eq 64 ] || return 1
    case "$fp" in
        *[!0-9a-f]*) return 1 ;;
    esac
    return 0
}

# validate_size N - true only for a plain non-negative integer.
validate_size() {
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

# Manifest slot storage: one set of plain (non-indirect) variables per
# fixed slot name, populated by load_manifest and read directly by the gate
# functions below via manifest_get_slot's literal case match. No eval
# anywhere in the manifest read/write path. slot_*_size is supplemental
# only (never read by any gate function); kept for parity with the on-disk
# record and possible future diagnostics.
slot_bin_emuwiz_cli_kind=""; slot_bin_emuwiz_cli_fp=""; slot_bin_emuwiz_cli_size=""
slot_bin_emuwiz_kind="";     slot_bin_emuwiz_fp="";     slot_bin_emuwiz_size=""
slot_alias_archivefs_cli_kind=""; slot_alias_archivefs_cli_fp=""
slot_alias_emuwiz_gui_kind="";    slot_alias_emuwiz_gui_fp=""
slot_alias_archivefs_gui_kind=""; slot_alias_archivefs_gui_fp=""
slot_desktop_kind=""; slot_desktop_fp=""; slot_desktop_size=""
slot_icon_32_kind="";  slot_icon_32_fp="";  slot_icon_32_size=""
slot_icon_64_kind="";  slot_icon_64_fp="";  slot_icon_64_size=""
slot_icon_128_kind=""; slot_icon_128_fp=""; slot_icon_128_size=""
slot_icon_256_kind=""; slot_icon_256_fp=""; slot_icon_256_size=""
slot_icon_512_kind=""; slot_icon_512_fp=""; slot_icon_512_size=""

# store_file_slot SLOT DIGEST SIZE - records a file slot's digest/size, but
# only if this slot has not already been seen in this parse (a second
# record for the same slot invalidates the whole manifest via parse_ok,
# never "last one wins"). Literal case match only - no eval, no
# indirection.
store_file_slot() {
    case "$1" in
        bin-emuwiz-cli)
            [ -z "$slot_bin_emuwiz_cli_kind" ] || { parse_ok=0; return 0; }
            slot_bin_emuwiz_cli_kind=file; slot_bin_emuwiz_cli_fp=$2; slot_bin_emuwiz_cli_size=$3 ;;
        bin-emuwiz)
            [ -z "$slot_bin_emuwiz_kind" ] || { parse_ok=0; return 0; }
            slot_bin_emuwiz_kind=file; slot_bin_emuwiz_fp=$2; slot_bin_emuwiz_size=$3 ;;
        desktop)
            [ -z "$slot_desktop_kind" ] || { parse_ok=0; return 0; }
            slot_desktop_kind=file; slot_desktop_fp=$2; slot_desktop_size=$3 ;;
        icon-32)
            [ -z "$slot_icon_32_kind" ] || { parse_ok=0; return 0; }
            slot_icon_32_kind=file; slot_icon_32_fp=$2; slot_icon_32_size=$3 ;;
        icon-64)
            [ -z "$slot_icon_64_kind" ] || { parse_ok=0; return 0; }
            slot_icon_64_kind=file; slot_icon_64_fp=$2; slot_icon_64_size=$3 ;;
        icon-128)
            [ -z "$slot_icon_128_kind" ] || { parse_ok=0; return 0; }
            slot_icon_128_kind=file; slot_icon_128_fp=$2; slot_icon_128_size=$3 ;;
        icon-256)
            [ -z "$slot_icon_256_kind" ] || { parse_ok=0; return 0; }
            slot_icon_256_kind=file; slot_icon_256_fp=$2; slot_icon_256_size=$3 ;;
        icon-512)
            [ -z "$slot_icon_512_kind" ] || { parse_ok=0; return 0; }
            slot_icon_512_kind=file; slot_icon_512_fp=$2; slot_icon_512_size=$3 ;;
    esac
    records_seen=$((records_seen + 1))
}

# store_symlink_slot SLOT TARGET - same duplicate-rejection discipline as
# store_file_slot, for the three symlink alias slots.
store_symlink_slot() {
    case "$1" in
        alias-archivefs-cli)
            [ -z "$slot_alias_archivefs_cli_kind" ] || { parse_ok=0; return 0; }
            slot_alias_archivefs_cli_kind=symlink; slot_alias_archivefs_cli_fp=$2 ;;
        alias-emuwiz-gui)
            [ -z "$slot_alias_emuwiz_gui_kind" ] || { parse_ok=0; return 0; }
            slot_alias_emuwiz_gui_kind=symlink; slot_alias_emuwiz_gui_fp=$2 ;;
        alias-archivefs-gui)
            [ -z "$slot_alias_archivefs_gui_kind" ] || { parse_ok=0; return 0; }
            slot_alias_archivefs_gui_kind=symlink; slot_alias_archivefs_gui_fp=$2 ;;
    esac
    records_seen=$((records_seen + 1))
}

# load_manifest - populates manifest_loaded=1, and the slot_* variables
# above, only from a manifest that passes every rule in the "PARSER"
# section of the OWNERSHIP MODEL comment above: single, matching headers;
# a declared record_count that exactly equals the number of valid,
# non-duplicate slot records actually found; a mandatory "end" line with
# nothing after it; and no line of any other shape anywhere. Any failure
# leaves manifest_loaded=0 and every slot_* empty - callers then use the
# no-manifest recognition rules for everything, never a partially-trusted
# mix, and never infer completeness from merely reaching EOF.
load_manifest() {
    manifest_loaded=0
    [ "$manifest_dir_kind" = dir ] || return 0
    [ -f "$manifest_file" ] || return 0
    [ ! -L "$manifest_file" ] || return 0

    manifest_version=""
    manifest_saved_bin_dir=""
    manifest_saved_data_home=""
    manifest_declared_count=""
    seen_schema=0
    seen_bindir=0
    seen_datahome=0
    seen_count=0
    seen_end=0
    records_seen=0
    parse_ok=1

    while [ "$parse_ok" -eq 1 ] && IFS=' ' read -r key rest; do
        # Nothing may follow "end" - not a record, not a header field, and
        # not a comment either. This guard runs before any dispatch at all,
        # so it applies uniformly to every line shape, not just the ones
        # that happened to have their own seen_end check.
        if [ "$seen_end" -eq 1 ]; then
            parse_ok=0
            continue
        fi
        case "$key" in
            '#'*)
                : # comments are allowed anywhere before "end" and never counted
                ;;
            schema_version)
                if [ "$seen_schema" -eq 1 ]; then parse_ok=0
                else seen_schema=1; manifest_version=$rest
                fi
                ;;
            bin_dir)
                if [ "$seen_bindir" -eq 1 ]; then parse_ok=0
                else seen_bindir=1; manifest_saved_bin_dir=$rest
                fi
                ;;
            data_home)
                if [ "$seen_datahome" -eq 1 ]; then parse_ok=0
                else seen_datahome=1; manifest_saved_data_home=$rest
                fi
                ;;
            record_count)
                if [ "$seen_count" -eq 1 ]; then parse_ok=0
                else seen_count=1; manifest_declared_count=$rest
                fi
                ;;
            end)
                seen_end=1
                ;;
            bin-emuwiz-cli|bin-emuwiz|desktop|icon-32|icon-64|icon-128|icon-256|icon-512)
                fld_kind=""; fld_fp=""; fld_size=""
                IFS=' ' read -r fld_kind fld_fp fld_size <<EOF
$rest
EOF
                if [ "$fld_kind" = file ] && validate_file_digest "$fld_fp" && validate_size "$fld_size"; then
                    store_file_slot "$key" "$fld_fp" "$fld_size"
                else
                    parse_ok=0
                fi
                ;;
            alias-archivefs-cli|alias-emuwiz-gui|alias-archivefs-gui)
                fld_kind=""; fld_target=""
                IFS=' ' read -r fld_kind fld_target <<EOF
$rest
EOF
                if [ "$fld_kind" = symlink ] && validate_symlink_fp "$fld_target"; then
                    store_symlink_slot "$key" "$fld_target"
                else
                    parse_ok=0
                fi
                ;;
            *)
                parse_ok=0
                ;;
        esac
    done <"$manifest_file"

    [ "$parse_ok" -eq 1 ] || return 0
    [ "$seen_schema" -eq 1 ] || return 0
    [ "$seen_bindir" -eq 1 ] || return 0
    [ "$seen_datahome" -eq 1 ] || return 0
    [ "$seen_count" -eq 1 ] || return 0
    [ "$seen_end" -eq 1 ] || return 0
    [ "$manifest_version" = "$manifest_schema_version" ] || return 0
    validate_size "$manifest_declared_count" || return 0
    [ "$manifest_declared_count" = "$records_seen" ] || return 0
    [ -n "$manifest_saved_bin_dir" ] || return 0
    [ "$manifest_saved_bin_dir" = "$bin_dir" ] || return 0
    [ -n "$manifest_saved_data_home" ] || return 0
    [ "$manifest_saved_data_home" = "$data_home" ] || return 0
    manifest_loaded=1
}

# manifest_get_slot SLOT - sets slot_kind/slot_fp to the manifest's record
# for SLOT (both empty if manifest_loaded=0 or SLOT has no record - which
# is the normal, supported shape of an intentionally partial manifest, not
# a parse failure). Maps SLOT to the corresponding slot_<name>_kind/_fp
# pair via a literal case match only - never eval, never indirection driven
# by file content.
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
# foreign. The gate reads PATH's CURRENT content and compares it against
# the OLD recorded digest from the manifest loaded at the start of this
# run - always strictly before any new content is ever written to PATH, so
# a legitimate reinstall/upgrade is validated against what was actually
# there, never against the new asset this run is about to install.
gate_binary() {
    slot=$1
    path=$2
    case $(path_kind "$path") in
        absent) printf 'absent\n'; return 0 ;;
        symlink|dir) printf 'foreign\n'; return 0 ;;
    esac
    manifest_get_slot "$slot"
    if [ "$slot_kind" = file ]; then
        current_fp=$(file_digest "$path") || current_fp=""
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
# Like gate_binary, the manifest comparison here always reads PATH's
# current (pre-overwrite) content against the OLD recorded digest.
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
            current_fp=$(file_digest "$path") || current_fp=""
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

# backup_foreign_path PATH - moves PATH (file, symlink, or directory) aside
# into a freshly, securely, exclusively created backup directory in the
# SAME PARENT as PATH (guaranteeing the same filesystem, so the move below
# is a plain rename, not a cross-device copy). mktemp -d's whole contract
# is that the name it returns did not exist a moment ago and cannot have
# been pre-planted by anything - there is no predictable name here for
# anything to race or pre-occupy, unlike a PID- or timestamp-derived name.
# The foreign object is then moved, under its own original basename, into
# that directory: since the directory was just created empty and exclusively
# by us, that destination is guaranteed not to exist yet either, so the
# move can never clobber anything (mv -n is added as a second, redundant
# guard rather than the only one). mv operates on PATH itself when PATH is
# a symlink - it is never dereferenced or followed. Sets BACKUP_PATH to the
# exact resulting location, which is the deterministic recovery
# instruction: the foreign object is never deleted, only moved, and stays
# exactly there - including if a later step in this same install run then
# fails, since nothing after this function ever touches BACKUP_PATH again.
backup_foreign_path() {
    path=$1
    parent=$(dirname -- "$path")
    base=$(basename -- "$path")
    backup_dir=$(mktemp -d -- "$parent/.emuwiz-foreign-backup.XXXXXXXX") ||
        fail "could not allocate a secure backup location next to: $path"
    chmod 0700 -- "$backup_dir" 2>/dev/null || true
    backup_target="$backup_dir/$base"
    mv -n -- "$path" "$backup_target" ||
        fail "could not move foreign path aside for backup (it has been left in place at: $path)"
    BACKUP_PATH="$backup_target"
}

# resolve_foreign_collision PATH - called only when a gate function reports
# "foreign". Without --replace-foreign: warns and returns 1 (caller must
# leave PATH alone). With --replace-foreign: backs PATH up via
# backup_foreign_path (never overwriting anything, never following a
# symlink) and returns 0 so the caller proceeds to install fresh.
resolve_foreign_collision() {
    path=$1
    if [ "$replace_foreign" -ne 1 ]; then
        warn "leaving foreign path untouched (not EmuWiz-owned): $path"
        warn "  re-run with --replace-foreign to move it aside and install here"
        skipped_any=1
        return 1
    fi
    backup_foreign_path "$path"
    warn "moved foreign path aside before installing: $path -> $BACKUP_PATH"
    return 0
}

# record_slot SLOT KIND PATH - appends a manifest record line for SLOT to
# $manifest_records_tmp, fingerprinting PATH fresh right now. Only ever
# called immediately after (re)installing PATH, so the recorded digest
# always reflects exactly what was just written - never the pre-existing
# object's digest, and never a digest computed before the write completed.
record_slot() {
    slot=$1
    kind=$2
    path=$3
    case "$kind" in
        file)
            digest=$(file_digest "$path") || fail "could not fingerprint installed file (SHA-256 unavailable or unreadable): $path"
            size=$(file_size "$path") || fail "could not determine the size of installed file: $path"
            printf '%s file %s %s\n' "$slot" "$digest" "$size" >>"$manifest_records_tmp"
            ;;
        symlink)
            target=$(readlink -- "$path") || fail "could not read installed symlink: $path"
            printf '%s symlink %s\n' "$slot" "$target" >>"$manifest_records_tmp"
            ;;
        *)
            fail "internal error: unknown manifest slot kind: $kind"
            ;;
    esac
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
    # behind, and only through a manifest_dir confirmed to be a real
    # directory and a manifest_file confirmed to be a plain regular file -
    # if something was left foreign, or the bookkeeping path itself is not
    # safe to touch, keep whatever is there so a future run (or a human)
    # still has it to work from.
    if [ "$left_foreign" -eq 0 ] && [ "$manifest_dir_kind" = dir ] \
        && [ -f "$manifest_file" ] && [ ! -L "$manifest_file" ]; then
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

ensure_manifest_dir
manifest_records_tmp=$(mktemp -- "$manifest_dir/.manifest-records.XXXXXX") ||
    fail "could not create a temporary file for the install manifest"

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

# Compose the manifest only now that record_count is known, then validate
# any pre-existing manifest_file before ever touching it: an existing
# manifest may only be replaced once it has been validated as belonging to
# THIS installation - which is exactly what load_manifest already
# established earlier in this run via manifest_loaded. A path that existed
# before this run but did NOT load successfully - whether it is a symlink,
# a directory, or simply a regular file whose content never validated as
# one of our own manifests (an unrelated file, a stale pre-bump schema, a
# hand-edited one that no longer parses) - is treated exactly like any
# other foreign collision in this script: left untouched and warned about
# by default, or backed up (never silently overwritten in place) with
# --replace-foreign. A manifest that WAS successfully loaded, or no
# pre-existing path at all, is always safe to replace outright - that is
# the normal, expected shape of every ordinary reinstall.
manifest_tmp=$(mktemp -- "$manifest_dir/.manifest.XXXXXX") ||
    fail "could not create a temporary file for the install manifest"
record_count=$(wc -l <"$manifest_records_tmp" | tr -d '[:space:]')
{
    printf '# EmuWiz installer ownership manifest - machine-generated, do not hand-edit.\n'
    printf '# Regenerated in full on every successful install/reinstall.\n'
    printf 'schema_version %s\n' "$manifest_schema_version"
    printf 'bin_dir %s\n' "$bin_dir"
    printf 'data_home %s\n' "$data_home"
    printf 'record_count %s\n' "$record_count"
    cat -- "$manifest_records_tmp"
    printf 'end\n'
} >"$manifest_tmp"
rm -f -- "$manifest_records_tmp"
chmod 0600 "$manifest_tmp"

manifest_write_blocked=0
manifest_preexisting=0
if [ -e "$manifest_file" ] || [ -L "$manifest_file" ]; then
    manifest_preexisting=1
fi
if [ "$manifest_preexisting" -eq 1 ] && [ "$manifest_loaded" -ne 1 ]; then
    if [ "$replace_foreign" -eq 1 ]; then
        backup_foreign_path "$manifest_file"
        warn "moved an unrecognised path at the ownership manifest location aside before installing: $manifest_file -> $BACKUP_PATH"
    else
        warn "leaving the ownership manifest untouched: $manifest_file exists but does not validate as belonging to this installation (found a $(path_kind "$manifest_file")); this run's installs above are not recorded until that is resolved"
        warn "  re-run with --replace-foreign to move it aside and record ownership here"
        manifest_write_blocked=1
    fi
fi
if [ "$manifest_write_blocked" -eq 1 ]; then
    rm -f -- "$manifest_tmp"
    skipped_any=1
else
    mv -f -- "$manifest_tmp" "$manifest_file"
fi

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
