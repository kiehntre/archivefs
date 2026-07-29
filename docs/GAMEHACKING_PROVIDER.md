# GameHacking.org provider

Status: developer-facing first adapter, enabled only from Cheats & Mods for a
PS2 game already present in the local ArchiveFS library. It is not a crawler,
mirror, bundled cheat database, or automatic background service.

## Workflow

ArchiveFS first inspects the selected local PS2 image. A verified PCSX2
executable CRC and serial are mandatory. The provider then reads the public PS2
index only to identify exact-title candidate game IDs, requests candidate game
pages one at a time, and stops at the first exact serial/CRC/region match.
Conflicting or incomplete identity fails closed.

After a match, an explicit Download or Refresh action sends the site's native
PCSX2 export request to `https://gamehacking.org/inc/sub.exportCodes.php` with
`format=PCSX2`, empty `codID`, the verified serial (or title) as `filename`,
`sysID=16`, the matched `gamID`, and `download=true`. ArchiveFS does not convert
encrypted formats or reinterpret unknown lines. Only strict PCSX2 `patch=`
lines enter the selectable install path.

The GUI previews individual cheats with their names, authors, and descriptions.
Selected entries are merged into a PNACH named for the verified local CRC. The
existing shared transaction engine provides preview, explicit replacement
confirmation, backup, journaled apply, and Undo. Existing user content is
preserved; a different destination file is never replaced without confirmation.

## Network and cache policy

- fixed `https://gamehacking.org` origin; redirects and proxies are disabled;
- descriptive ArchiveFS/GitHub User-Agent;
- one request at a time, three-second default delay, bounded connect/global/body
  timeouts, bounded response sizes, and at most three attempts;
- exponential backoff only for HTTP 429 and temporary 5xx/transport failures;
- permanent 4xx responses are not retried, and access denial stops cleanly;
- public `robots.txt` is checked before provider page retrieval;
- game pages and native exports are cached below the private ArchiveFS data
  directory; Refresh deliberately replaces the relevant cache entries;
- response bodies are cached byte-for-byte. HTML uses the declared HTTP
  charset when recognised and otherwise uses safe lossy UTF-8 decoding, so a
  legacy Windows-1252 title cannot make the complete index unusable;
- leaving or changing the selected-game workflow drops the worker receiver, so
  stale results cannot become installable.

No request contains a ROM, ROM hash, local path, emulator profile, installed
cheat file, or ArchiveFS database content. The provider sends only the public
site form fields described above. It does not log in, bypass access controls,
solve CAPTCHA, fetch concurrently, enumerate the full site, or offer an
unlimited mode.

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
Downloaded pages and cheat data are cache data only and are never release
assets or repository content. Users remain responsible for permission to use
third-party cheat data; ArchiveFS does not redistribute the cache.
