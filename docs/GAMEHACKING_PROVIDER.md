# GameHacking.org provider

Status: developer-facing first adapter, enabled only from Cheats & Mods for a
PS2 game already present in the local ArchiveFS library. Its explicit CLI
indexer is a bounded PS2-only crawler; it is not a site mirror, bundled cheat
database, or automatic background service.

## Workflow

ArchiveFS first inspects the selected local PS2 image. A verified PCSX2
executable CRC is mandatory; verified serial and region evidence are used when
available. Runtime matching reads a deterministic local catalogue built from
every numbered public PS2 index page; it performs no
live discovery request. Matching priority is exact normalized serial plus CRC,
exact serial plus compatible region, then exact CRC. A normalized-title match
is only a displayed candidate and cannot trigger
an export until the user confirms its title, serial, region, and GameHacking
game ID.

Build or resume the catalogue with:

```text
archivefs-cli gamehacking-ps2-index-refresh --resume
```

`--cache-root <absolute-path>` and `--json` are also supported. Progress is
written page by page. Index HTML is fetched serially with a two-second delay;
already cached pages are parsed without being downloaded again, so an
interrupted run resumes from the first missing page. The catalogue JSON is
sorted by game ID and records each game page URL, index page URL, retrieval
timestamp, serial, region, and CRC. Indexing never requests PNACH exports.

After a match, an explicit Download or Refresh action sends the site's native
PCSX2 export request to `https://gamehacking.org/inc/sub.exportCodes.php` with
`format=PCSX2`, empty `codID`, the verified serial (or title) as `filename`,
`sysID=16`, the matched `gamID`, and `download=true`. ArchiveFS does not convert
encrypted formats or reinterpret unknown lines. Only strict PCSX2 `patch=`
lines enter the selectable install path.

The GUI previews individual cheats with their names, authors, and descriptions.
Native PCSX2 exports encode those names in `[category\title]` section headers,
with `author=` and possibly multiline `description=` metadata before each
block's `patch=` lines. ArchiveFS retains each section as a separate cheat,
shows the readable category and title as its primary label, and uses `Cheat N`
only when neither a section header nor a trustworthy PNACH comment provides a
name.

Selected entries are merged into a PNACH named for the verified local CRC. The
existing shared transaction engine provides preview, explicit replacement
confirmation, backup, journaled apply, and Undo. Existing user content is
preserved; a different destination file is never replaced without confirmation.

## Network and cache policy

- fixed `https://gamehacking.org` origin; redirects and proxies are disabled;
- descriptive ArchiveFS/GitHub User-Agent;
- one request at a time, a two-second index delay (three seconds for ordinary
  provider requests), bounded connect/global/body timeouts, bounded response
  sizes, and at most three attempts;
- exponential backoff only for HTTP 429 and temporary 5xx/transport failures;
- permanent 4xx responses are not retried, and access denial stops cleanly;
- public `robots.txt` is checked before provider page retrieval;
- numbered index pages, deterministic catalogue JSON, and native exports are
  cached below the private ArchiveFS data directory; GUI Refresh deliberately
  replaces only the selected game's export;
- response bodies are cached byte-for-byte. HTML uses the declared HTTP
  charset when recognised and otherwise uses safe lossy UTF-8 decoding, so a
  legacy Windows-1252 title cannot make the complete index unusable;
- leaving or changing the selected-game workflow drops the worker receiver, so
  stale results cannot become installable.

No request contains a ROM, ROM hash, local path, emulator profile, installed
cheat file, or ArchiveFS database content. The provider sends only the public
site form fields described above. It does not log in, bypass access controls,
solve CAPTCHA, fetch concurrently, enumerate systems other than the bounded PS2
index, or download exports in bulk.

## Architecture and provenance

`GameHackingSystemAdapter` isolates system ID, index URL, export format, and
identity support. `Ps2GameHackingAdapter` is the first implementation; another
system can add an adapter without changing the cache, transport, retry, or
transaction layers. HTML selectors and PNACH parsing live in the provider
module and fixture tests perform no network access.

The `scraper` crate is used because GameHacking.org currently exposes HTML,
not a supported official API. It provides a bounded standards-aware HTML tree
and keeps selectors in one repairable module instead of relying on fragile
string slicing. A future official API should replace only discovery and page
parsing while retaining identity matching, provenance, selection, and the
shared transaction boundary.

GameHacking game ID, source URL, author, and description remain attached to
provider records and are written as comments in ArchiveFS-managed PNACH blocks.
Downloaded pages, catalogue JSON, and cheat data are cache data only and are
never release assets or repository content. Users remain responsible for
permission to use third-party cheat data; ArchiveFS does not redistribute the
cache.
