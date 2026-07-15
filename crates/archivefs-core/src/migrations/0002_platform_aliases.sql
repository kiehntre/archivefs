-- Migration 0002: persistent custom platform folder aliases.
--
-- One row per user-defined mapping from a normalized folder-name alias to
-- a canonical platform (see crate::canonical_platform_names). Consulted
-- during scanning at a strictly higher precedence than the built-in
-- FOLDER_PLATFORM_ALIASES table, but lower than a manual archive
-- assignment - see provenance_priority in database.rs.
--
-- `alias` keeps the user's original typed text (trimmed) for display
-- only; matching always goes through `normalized_alias` (the same
-- normalize_path_segment normalization the built-in folder alias table
-- already uses - ASCII alphanumeric only, lowercased), which is what
-- this table's uniqueness constraint is enforced on. `platform` always
-- stores the canonical spelling from canonical_platform_names(), never
-- free-form/custom text - see Database::add_platform_alias.
CREATE TABLE platform_aliases (
    id               INTEGER PRIMARY KEY,
    alias            TEXT NOT NULL,
    normalized_alias TEXT NOT NULL,
    platform         TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE UNIQUE INDEX platform_aliases_normalized_alias ON platform_aliases(normalized_alias);
