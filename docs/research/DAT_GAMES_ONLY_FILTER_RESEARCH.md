# DAT Games-Only Filtering Research

Status: research and design only
Research date: 2026-08-10

## 1. Executive summary

EmuWiz can offer a safe **Games only** view, but only if content filtering is a
derived selection policy rather than a change to an imported DAT. The complete
upstream catalogue must remain available to matching and audit, every normalized
classification must retain its evidence and original source value, and uncertain
entries must remain visible as **Unknown / needs review**.

The decisive constraint is that upstream databases often know more than their
standard DAT exports contain. Redump has a strong category vocabulary in its
submission database, and No-Intro DAT-o-MATIC has structured archive fields, but
EmuWiz's current Logiqx parser retains neither. TOSEC is the best near-term source
for classification from files already available to EmuWiz because its DATs are
split into named content categories and its canonical entry names carry typed,
delimited flags. Generic Logiqx and ClrMamePro files are the hardest: format alone
does not establish content type.

The recommended model has two layers:

1. A normalized primary content class, used by the three beginner-facing filters.
2. Orthogonal qualifiers and evidence, used to preserve distinctions such as
   release stage, licensing, media role, relation to a base game, and confidence.

**Bottom line**

- **Can Games only be implemented safely?** Yes, if it includes only explicitly
  or strongly classified games, keeps Unknown visible, and never alters identity
  matching or imported DAT data. It is not safe as a broad filename-substring
  filter.
- **Easiest source:** TOSEC from current-style local DATs. Redump is also easy
  when its structured website category is supplied with the DAT, but not from a
  normal Redump DAT alone.
- **Hardest sources:** generic Logiqx/ClrMamePro, normal Redump DAT exports without
  an enriched sidecar, and OMNI_DAT imports when the browser's `Type` metadata is
  not delivered in a documented machine-readable form.
- **Games only includes:** final games, game compilations, and every required
  part/disc of those games. Homebrew, public-domain, and unlicensed works remain
  included when their content is genuinely a game; those labels are qualifiers,
  not reasons to hide a title.
- **Unknown:** always counted and shown in a review bucket; never silently treated
  as non-game and never used for automatic rename/organisation under a restrictive
  filter. **Everything** includes it in the main result set.
- **Upstream semantics:** unchanged. Classification is an EmuWiz annotation and
  filtering is presentation/selection policy only.

## 2. Current EmuWiz DAT model

### What is supported now

The provider-neutral model in
[`crates/archivefs-core/src/dat/model.rs`](../../crates/archivefs-core/src/dat/model.rs)
recognizes:

- Logiqx XML: generic, No-Intro, Redump
- ClrMamePro text: generic, TOSEC

`DatSource` retains format, detected ecosystem, file path, name, description,
version, author, homepage, raw ClrMamePro header, counts, and parse warnings.
`DatGameEntry` has names, ROMs, clone/sample relationships and several MAME-like
fields. `DatRomEntry` has file identity, size, hashes, dump status, merge, and date.

There is no current normalized content type, source category, media type, release
type, tag collection, or raw provider-metadata map. OMNI_DAT is not a distinct
`DatEcosystem` variant.

### What the parsers actually retain

The presence of fields in the provider-neutral model does not mean the current
parsers populate them:

- The Logiqx parser retains source header fields; game name, description and
  `cloneof`; and ROM identity/status fields. It does not retain category,
  release/media metadata, or provider-specific fields from enriched No-Intro or
  Redump data. Several existing `DatGameEntry` fields are left empty.
- The ClrMamePro parser retains the header (including its raw text), game name,
  description, `cloneof`/`romof`, and ROM identity/status fields. It does not parse
  TOSEC Naming Convention flags into structured values.
- Region, language, revision, and some release clues are currently parsed later
  from canonical names for candidate policy, not preserved as upstream content
  metadata.

This matters because a classifier must not claim that an upstream field is
available merely because the upstream website stores it.

### Existing policy and audit boundaries

The current DAT policy ranks already matched candidates by source priority,
region, language, revision, and parent/clone preference. Content visibility is
orthogonal: it should be a separate policy value and must not become an identity
score or a way to erase competing hash matches.

The audit model reports a verdict per local file (`Exact`, multiple exact,
`Probable`, filename-only, ambiguous, not in DAT, or no evidence). It must continue
to audit against the complete catalogue. A matched application hidden from the
library view is still an exact DAT match, not “Not in DAT.”

Rename proposals are derived from verified matches and policy winners;
organisation plans separately mark entries suggested, blocked, unsupported, or in
conflict. Both downstream stages need an explicit content-eligibility gate so an
excluded application, firmware entry, or Unknown candidate cannot supply an
automatic canonical name or destination.

### Recommended location

Classification should live in a new provider-neutral DAT classification layer
after parsing and before presentation/selection. Parsers should preserve raw
structured metadata and provenance; source adapters should normalize it; filters
should consume the normalized result. `ParsedDat.games` must not be shortened or
rewritten.

Conceptually:

```text
raw DAT + optional trusted source descriptor
                 |
                 v
        lossless provider parse
                 |
                 v
 normalized class + qualifiers + evidence
        |                         |
        v                         v
 complete identity audit     content selection policy
        |                         |
        +----------+--------------+
                   v
       UI / rename / organisation eligibility
```

The annotation should be keyed to stable in-memory source/entry identity, not to a
rewritten title. Technical details must be able to show the raw upstream field,
the normalized result, the rule and confidence that produced it.

## 3. Source-by-source metadata matrix

Legend:

- **Entry**: structured per-entry metadata exists.
- **DAT**: structured or canonical category at source/DAT level.
- **Name**: defined, delimited naming convention; weaker than structured data.
- **Upstream only**: present in the upstream database/site but not reliably in a
  standard local DAT or not retained by EmuWiz today.
- **No contract**: observed in a browser but no stable public import schema was
  found.

| Source | System/platform | Category/type | Media/release type | Canonical naming/tags | Relationships | Region/language | Demo/beta/proto/sample | Apps/utilities/magazines | Digital/DLC/theme separation | Current EmuWiz availability |
|---|---|---|---|---|---|---|---|---|---|---|
| No-Intro | Normally DAT-level; richer database fields exist | Some DAT separation and upstream category work; no universal category in ordinary exports | Physical/digital and release fields exist upstream; export-dependent | Published ordered flag grammar; some values also exist as custom-XML fields | P/Clone data exists and `cloneof` is retained when exported | Entry fields in DAT-o-MATIC/custom XML; canonical name flags in ordinary DATs | Structured `archive_devstatus`, release and BIOS fields upstream; canonical `(Beta)`, `(Proto)`, `(Sample)` and `[BIOS]` names | No dependable universal application/utility/magazine type in ordinary DATs | Often separate systems/DAT sets, such as digital updates and DLC; not a universal per-entry field in current imports | Source identity, names, `cloneof`, hashes. Most rich fields are currently lost |
| Redump | DAT-level system and structured upstream submission field | Strong upstream category list: Games, Applications, Audio, Bonus Discs, Coverdiscs, Demos, Educational, Multimedia, Preproduction, Video, Add-Ons | Disc/media details and release/version fields exist upstream | Detailed title/version/disc conventions, but no equivalent universal content tag in ordinary DAT names | Parent/clone is not the main content taxonomy; related-disc facts exist in records/conventions | Structured upstream; commonly rendered in names | Category and upstream metadata are strong; some canonical name/version labels exist | Explicit upstream categories cover these cases | Add-Ons is explicit upstream; other digital distinctions are system/category dependent | Standard DAT gives source header, names, files and hashes, but normally not website category; current parser loses rich metadata |
| TOSEC | DAT filename/header encodes branch, platform and often media/category | Strong canonical DAT-level splits such as Games, Applications, Coverdiscs, Magazines, Multimedia, Operating Systems, Samplers | Canonical entry flags include media type/number; TOSEC/TOSEC-ISO/TOSEC-PIX split media/resource families | Formal TNC grammar with demo, date, publisher, system, video, country, language, copyright, development, media and dump-status fields | ClrMamePro relationships may occur but category is generally source-level | Canonical country/language flags | Canonical demo and development-status fields include kiosk/playable/rolling/slideshow and alpha/beta/preview/prototype/pre-release | Explicit project and DAT categories | No single universal DLC/theme field; use an explicit DAT category when present, otherwise Unknown | Raw source/header and names are retained; category and TNC flags are not yet normalized |
| OMNI_DAT | Browser exposes Group, Ecosystem and Platform | Browser exposes DAT-level `Type` | Browser exposes Format and DAT metadata; entry-level content schema not publicly documented | Depends on underlying ecosystem; no public OMNI entry-tag contract found | OMNI_TITLES/OmniGames links are visible conceptually, but no public import contract was found | Catalogue ecosystem dependent | Catalogue ecosystem dependent | `Type` may classify a DAT, subject to a documented export/sidecar | Site has distinct update-related views, but no stable public OMNI_DAT field contract was found | Not detected as its own ecosystem; only underlying generic/known DAT format can be parsed |
| MAME software lists / `-listxml` | Strong machine/software-list/interface structure | Base software-list XML does not provide a universal game/application category | Parts and interfaces represent media; machine XML has device/runnable/BIOS-related structure | Structured XML attributes and free-form `info`; no universal content tag | `cloneof`, `romof`, device relations and software parts are structured | Description/year/publisher and optional info fields; language may appear as info | Status and relationships exist, but no universal release-stage vocabulary across all lists | Needs list-level knowledge or an enriched category dataset | Not universal | Not a first-class current ecosystem; generic formats may parse only a subset |
| Libretro database | Database/RDB is system-oriented | Separate metadata DATs can supply genre and other attributes | Varies by source DAT | Inherits naming/tags from source datasets plus separate metadata files | Depends on joined metadata | Region/language commonly inherited from source naming/metadata | Depends on source DAT | Genre/developer and other sidecar metadata can help, but are not a universal content-type contract | Depends on source | Not a first-class current ecosystem and no current metadata-sidecar join |

### No-Intro

No-Intro's official naming documentation specifies the sequence `[BIOS flag]
Title (Region) (Languages) (Version) (Devstatus) (Additional) (Special) (License)
[Status]`. It also documents custom-XML/website fields for region, languages,
development status, BIOS, licensing, physical/digital state, release completeness,
and P/Clone relationships. `(Beta)`, `(Proto)`, and `(Sample)` have defined
meanings. These are good signals when supplied as structured fields, and acceptable
medium-confidence signals when parsed strictly as canonical, delimited No-Intro
flags.

However, the documentation explicitly distinguishes website/custom-XML fields
from internal storage and ordinary output. EmuWiz must capability-detect the
actual import and must not assume that every No-Intro DAT contains those fields.
Dataset-level separation, including digital/update/DLC sets, can be high confidence
when the source descriptor is authoritative.

### Redump

Redump's submission guidance exposes a strong category vocabulary: Add-Ons,
Applications, Audio, Bonus Discs, Coverdiscs, Demos, Educational, Games,
Multimedia, Preproduction, and Video. Its conventions also distinguish
compilations, covermounts, demos, press/promotional material, and other non-retail
discs.

This makes Redump straightforward to map **at the upstream database layer**. The
important limitation is delivery: the documented DAT download parameters and
ordinary Redump DAT content do not reliably carry the website category. A normal
Logiqx DAT therefore should not be classified as `Game` merely because Redump is
game-oriented. EmuWiz needs a trusted sidecar/export field for high-confidence
category mapping. Without it, strict canonical tags may identify a few extras,
but most entries remain Unknown.

### TOSEC

TOSEC explicitly catalogs applications, BIOS, compilations, coverdisks, demos,
device drivers, educational software, games, magazines, multimedia, operating
systems, and promotional/sampler software. Its branches separate non-optical
software, optical software/firmware, and scans/resources. Release DAT names are
commonly category-specific, while the TOSEC Naming Convention provides delimited
entry fields for demo kind, development status, country/language, media type and
number, and other flags.

That combination makes TOSEC the safest P0 adapter: classify the whole source from
a recognized canonical header/DAT name, then refine release/media qualifiers from
strict TNC tokens. A token parser must be grammar-based; a substring such as
“demo” inside a title is not evidence.

### OMNI_DAT

The public OMNI_DAT browser exposes DAT-level columns for Group, Ecosystem, Type,
Platform, DAT, Footprint, Summary, Format, Version, update time, OmniGames and
Status. This is promising source-level classification metadata. The public
materials reviewed did not establish a stable, machine-readable entry schema or
guarantee that browser `Type` accompanies an imported DAT.

The design should therefore accept an official, versioned OMNI source descriptor
or sidecar if one becomes available. It should not scrape the browser or infer
entry type from OMNI branding. Confidence in the browser-level observation is
high; confidence in an implementable local import contract is low.

### MAME and Libretro

MAME is relevant as a major catalogue vocabulary and as an OMNI_DAT source. Its
software-list model has structured software, parts/interfaces, relationships and
machine/device flags, but no universal base field that separates games from
applications across computer platforms. An adapter can reliably use explicit
device/runnable/BIOS facts and curated list-level metadata; it cannot declare all
runnable software to be games.

Libretro's database project compiles system databases and separate metadata DATs.
Those sidecars can add genres, developer and other useful facts after a reliable
join, but current EmuWiz has neither a first-class Libretro DAT ecosystem nor that
join. This is a P1 adapter, not a basis for P0 filename rules.

## 4. Proposed normalized content classes

A single enum cannot safely carry every independent fact. Use a primary
`ContentClass` plus qualifiers and evidence.

| Normalized class | Meaning |
|---|---|
| `Game` | A complete playable game, including explicitly game-classified educational, homebrew, public-domain or unlicensed releases |
| `GameDemo` | A playable/non-playable demonstration of a game or game system |
| `GameBetaPrototype` | Alpha, beta, prototype, preview, preproduction or pre-release game content |
| `GamePromoSample` | Press sample, kiosk build, sampler, prize/promotion, or other game promotional build |
| `GameCompilation` | A compilation explicitly known to contain complete games |
| `MixedCompilation` | A compilation with mixed/uncertain content or no proof that its components are complete games |
| `Application` | Non-game end-user software |
| `UtilityDriver` | Utility, diagnostic, device driver, development tool or similar support software |
| `Educational` | Educational/edutainment content not explicitly established as a game |
| `Magazine` | Magazine, book, catalogue, comic or comparable publication/resource |
| `Coverdisc` | Magazine coverdisc/coverdisk or covermount; content may be mixed |
| `MusicAudio` | Standalone audio/music content |
| `VideoMultimedia` | Video or non-game multimedia content |
| `BonusDisc` | A separate bonus/making-of/art/media disc associated with another release |
| `FirmwareSystem` | BIOS, firmware, operating system, system software or device image |
| `DLCAddon` | Downloadable content or an add-on that depends on a base title |
| `UpdatePatch` | Update, patch or upgrade that is not independently playable |
| `ThemeAvatar` | Theme, avatar, icon or other cosmetic account/system content |
| `DocumentationResource` | Manual, scan, artwork or other non-executable preservation resource |
| `Unknown` | Insufficient or conflicting evidence; never a synonym for non-game |

Required qualifiers:

- `release_stage`: final, demo, alpha, beta, prototype, preview, sample, promo,
  kiosk, unknown.
- `licensing`: commercial, licensed, unlicensed, homebrew, public-domain, unknown.
- `media_role`: main/play disc, install disc, data disc, supplemental disc, bonus
  disc, dependency, unknown.
- `collection_form`: single title, multidisc title, game compilation, mixed
  compilation, coverdisc, unknown.
- `game_relation`: base game identifier when explicit, game-related without a
  resolvable base, standalone, unknown.
- original system, region/language, media and source tags, kept independently of
  content class.

The qualifiers prevent category mistakes. “Homebrew” is not a content class: a
homebrew game is still `Game`, while a homebrew diagnostic tool is
`UtilityDriver`. “Multidisc” is not a content class: all required discs inherit
the package's class and are selected atomically.

## 5. Games only / Games + extras / Everything rules

### Games only

Automatically select:

- `Game`
- `GameCompilation`
- every required disc/part of those entries

Do not automatically select the remaining classes. Show `Unknown` separately as
needs review. This conservative rule deliberately excludes demos, prototypes and
promotional builds: preservation makes them valuable, but they are extras rather
than normal library games.

An educational, homebrew, public-domain or unlicensed title is included when
structured evidence classifies its content as a game. Weak naming evidence must
not hide or include it automatically.

### Games + extras

Select everything in Games only, plus:

- `GameDemo`, `GameBetaPrototype`, `GamePromoSample`
- `MixedCompilation`, `Educational`, `Coverdisc`, `BonusDisc`
- `DLCAddon`, `UpdatePatch`, `ThemeAvatar`
- `MusicAudio` or `VideoMultimedia` only when trusted metadata explicitly says it
  is related to a game; standalone audio/video remains Everything-only

Unknown remains in the visible needs-review bucket rather than being silently
selected. Applications, utilities, magazines/publications, firmware/system
software, documentation resources and standalone audio/video remain
Everything-only.

### Everything

Select every class, including `Unknown`. “Everything” does not change normalized
classes or confidence; it only changes eligibility/presentation.

### Policy truth table

| Class | Games only | Games + extras | Everything |
|---|---:|---:|---:|
| Game, GameCompilation and required parts | Yes | Yes | Yes |
| Demo, beta/prototype, promo/sample | No | Yes | Yes |
| MixedCompilation, Educational, Coverdisc, BonusDisc | No | Yes | Yes |
| DLCAddon, UpdatePatch, ThemeAvatar | No | Yes | Yes |
| Game-related MusicAudio/VideoMultimedia | No | Yes | Yes |
| Application, UtilityDriver, Magazine, FirmwareSystem, DocumentationResource | No | No | Yes |
| Standalone MusicAudio/VideoMultimedia | No | No | Yes |
| Unknown | Review bucket | Review bucket | Yes |

The initial user default should be **Games only**, provided the UI always displays
the Unknown count beside it. Existing installations should migrate to
**Everything** until a classification preview is accepted, avoiding an apparent
loss of previously visible material.

## 6. Confidence/fallback model

Each result should carry:

- normalized class and qualifiers
- confidence: `High`, `Medium`, or `Low`
- evidence kind and exact raw value
- source ecosystem and source revision/version
- classifier rule identifier/version
- optional conflicting evidence

Evidence precedence, strongest first:

1. Explicit structured entry category/type from a documented source export.
2. Explicit structured source/DAT category from a documented export or signed/
   versioned sidecar.
3. A recognized canonical source header or DAT filename whose grammar defines a
   category, particularly TOSEC category splits.
4. Strictly parsed, delimited tokens from the source's published naming grammar,
   such as No-Intro `(Proto)` or TOSEC demo/development fields.
5. A conservative filename hint, only where the source has no structured signal.
6. No evidence: `Unknown`.

Rules:

- Structured metadata always wins over filename guessing.
- Generic substrings, directory names chosen by the user, file extensions, and
  platform stereotypes are not sufficient evidence.
- A recognized canonical token can refine release stage but must not manufacture
  a base `Game` classification when source category is unknown. For example,
  `(Proto)` proves prototype status, not necessarily that a computer-platform
  program is a game.
- Conflicting high-confidence evidence yields `Unknown` plus a conflict, unless a
  documented provider-specific precedence rule resolves it.
- Low-confidence evidence may suggest a class in technical details, but must not
  make an entry eligible for automatic rename or organisation under restrictive
  modes.
- Manual overrides are local annotations with actor/time/reason and never rewrite
  the source DAT.

## 7. Edge cases

| Edge case | Safe treatment |
|---|---|
| Demo disc containing a full unlockable game | Honor the upstream Demo category. Put it in Games + extras; allow a reviewed local override. Do not inspect unlock mechanics or infer “full game” from anecdotes. |
| Compilation disc | `GameCompilation` only when structured evidence says it contains complete games; otherwise `MixedCompilation`. Never split or rename components without explicit component identity. |
| Magazine coverdisc | `Coverdisc`, not `Magazine`; Games + extras. Its presence beside a magazine does not establish that every program on it is a game. |
| Kiosk/promotional disc | `GamePromoSample` when explicitly game-related; otherwise preserve the source category or Unknown. |
| Prototype/beta | `GameBetaPrototype` only when the base content is known to be a game. Otherwise keep the primary class/Unknown and record the release-stage qualifier. |
| BIOS/firmware | `FirmwareSystem`; Everything-only. A required emulator dependency is not a library game. |
| Applications on computer platforms | `Application`; Everything-only. Never assume all runnable files on a computer platform are games. |
| Public-domain/homebrew/unlicensed | Treat as licensing qualifiers. Include in Games only when independently classified as `Game`. |
| Educational title | If upstream says Games, classify as `Game`; if it only says Educational, use `Educational` and Games + extras. |
| Game-related bonus/video disc | `BonusDisc`, or audio/video with explicit game relation; Games + extras. Standalone video/audio is Everything-only. |
| Multidisc game | Classify the package/title, inherit to every required disc, and select/organize atomically. Disc 2 is not a bonus disc merely because it is not bootable alone. |
| DLC/update/theme/avatar | Keep dependency and subtype. Games + extras, never masquerading as the base game. An orphaned dependency remains visible and non-actionable. |
| Redump non-retail disc | Use Redump's explicit category if delivered. Retail status alone does not decide whether something is a game. |
| TOSEC software category | Prefer canonical DAT category/header over entry-name inference; parse TNC flags only as refinements. |
| Same hash in game and non-game entries | Report all exact candidates. Do not use the visibility filter to falsify identity or silently break the tie; block automatic downstream action unless normal identity policy safely resolves it. |

## 8. UI proposal

Add a **Library content** control to DAT source policy, independent of region,
language, revision, clone and source-priority controls:

- **Games only** — Complete games and game compilations.
- **Games + extras** — Also demos, prototypes, promotions, bonus content, add-ons
  and other explicitly game-related material.
- **Everything** — Applications, magazines, firmware, multimedia and unclassified
  entries too.

Before applying a change, show a non-destructive preview:

```text
Games                         8,412
Game extras                     936
Other content                   284
Needs review (Unknown)          117
```

The Unknown row must remain visible in all modes and open a review list. Avoid
wording such as “removed”; use “not selected by this view.” A per-source details
view should show:

- original provider and DAT name/version
- original category/type/media/release values exactly as imported
- normalized class and qualifiers
- confidence and rule/evidence
- whether the entry is selected by the current content policy
- conflicts or local override history

Support a global default plus optional platform/source overrides, matching the
shape of existing effective DAT policy. A filter change must preview its impact
and invalidate stale rename/organisation plans.

## 9. Interaction with matching, rename and organisation

### Matching and audit

- Build identity indexes from every upstream entry, regardless of content filter.
- Resolve hash/size/filename evidence exactly as today.
- Attach content classification to candidates and results; do not alter an audit
  verdict because a match is hidden from the primary library view.
- Preserve all candidates in multiple-match results, including excluded classes.
- Do not use content eligibility as a hidden identity tie-breaker.

### Counts

Keep current audit verdict counts unchanged and add an orthogonal content summary:

- matched by normalized class
- selected by active filter
- not selected by active filter
- Unknown / needs review
- classification conflicts

Maintain separate counts for the imported upstream catalogue and audited local
files. The UI must not confuse “117 Unknown catalogue entries” with “117 local
files matched to Unknown entries.” Add conservation checks so every applicable
entry is selected, not selected, or Unknown/review, with no unreported loss.

### Rename and organisation

- First establish identity using the complete catalogue.
- Then apply content eligibility to downstream action planning.
- An excluded or Unknown entry may be shown as a verified audit match, but it must
  not generate an automatic rename or organisation suggestion under Games only or
  Games + extras.
- If exact candidates span eligible and excluded classes, preserve the ambiguity;
  filtering must not erase the excluded candidate to manufacture a winner.
- Store the active content policy and classifier version in plan provenance. A
  policy, override, source metadata or classifier-version change makes the plan
  stale and requires regeneration.
- Existing rename/organisation transaction journals remain authoritative and must
  not be reinterpreted by later classifier changes.
- Group required multidisc files before eligibility and plan them atomically.

A new non-actionable reason such as `ExcludedByContentPolicy` or
`UnclassifiedContent` is preferable to folding these cases into generic
unsupported/ambiguous counts. This keeps the UI honest about why a verified file
was not proposed.

## 10. Tests needed

### Parser and provenance

- Fixtures for every structured category/media/release field an adapter claims to
  support; verify raw value round-trips unchanged.
- Capability tests proving an ordinary DAT without a field does not gain one from
  source assumptions.
- No-Intro custom-XML and ordinary Logiqx variants; strict canonical token tests.
- Redump standard DAT versus enriched category sidecar.
- TOSEC source/header category grammar and every demo/development/media token.
- OMNI descriptor schema tests only after an official versioned contract exists.

### Classification

- Table-driven mapping for every normalized class and filter mode.
- Evidence precedence, equal-strength conflicts, Unknown fallback and audited
  manual override behavior.
- Negative tests for title substrings such as a game whose proper title contains
  “Demo”, “Magazine”, “Utility”, or “BIOS”.
- Homebrew/public-domain/unlicensed game versus utility.
- Educational game versus untyped educational software.
- Related and standalone audio/video.
- Multidisc inheritance and atomic selection.
- Game, mixed and unknown compilations.
- DLC/update/theme dependency and orphan behavior.

### Audit and downstream safety

- Audit verdict and total remain identical across all three filters.
- Filtered exact match is never reported as NotInDat.
- Same-hash cross-class candidates remain multiple/ambiguous.
- Excluded and Unknown candidates cannot produce automatic rename/organisation
  proposals in restrictive modes.
- Everything restores eligibility without changing class or audit truth.
- Content counts conserve totals and distinguish catalogue entries from local
  audit matches.
- Policy/classifier changes invalidate plans; old transaction journals remain
  readable and unchanged.
- Technical details display original upstream metadata and classification evidence.

## 11. P0/P1 implementation plan

### P0: safe useful filter

1. Extend the provider-neutral parsed representation to retain optional raw
   category/type, media/release, system, tags and source provenance without
   changing existing serialized/persisted semantics.
2. Add a derived classification module with normalized class, qualifiers,
   confidence, evidence and a versioned rule identifier. Never mutate
   `ParsedDat.games`.
3. Implement conservative adapters:
   - TOSEC canonical source-category/header grammar and strict TNC qualifiers.
   - No-Intro structured custom fields when present, plus strictly delimited
     documented tokens.
   - Redump category only from an explicit trusted field/descriptor; otherwise
     conservative qualifiers and Unknown.
   - generic formats default to Unknown unless they carry an explicit recognized
     category.
4. Add an orthogonal `GamesOnly` / `GamesAndExtras` / `Everything` effective
   policy, preview counts, visible Unknown review bucket and migration behavior.
5. Keep audit/index complete; gate rename and organisation after identity
   resolution; record policy/classifier provenance and explicit exclusion reasons.
6. Add the P0 parser, classification, conservation and downstream safety tests.

### P1: richer source adapters and review tools

1. Define and validate versioned metadata sidecars for Redump and No-Intro
   enriched exports; support an official OMNI_DAT descriptor if published.
2. Add MAME software-list/machine adapters with structured device/BIOS facts and
   curated list-level categories.
3. Add Libretro metadata joins only with stable identity and provenance.
4. Add dependency/package grouping for DLC, updates, bonus media and multidisc
   sets where sources provide explicit relations.
5. Add audited local overrides, bulk review, conflict diagnostics and exportable
   classification reports.
6. Measure Unknown rates per source and improve only with documented,
   regression-tested rules; never optimize the metric by aggressive guessing.

## 12. Sources and confidence notes

Primary sources consulted:

- [No-Intro Naming Convention](https://wiki.no-intro.org/index.php?title=Naming_Convention) — **high confidence** for documented canonical/custom-XML fields and token meanings; **medium confidence** for availability in any particular DAT export because the page distinguishes form/custom XML fields from internal/database representation.
- [No-Intro General DAT Notes](https://wiki.no-intro.org/index.php?title=General_dat_notes) — **medium confidence** for current DAT-o-MATIC practices and richer media/archive metadata; export settings and systems vary.
- [No-Intro website requests and ideas](https://wiki.no-intro.org/index.php?title=Website_requests_and_ideas) — **medium confidence** evidence that category/filter fields have evolved and that not all website fields are universally present in DAT output.
- [No-Intro 3DS Digital Updates and DLC example](https://wiki.no-intro.org/index.php?title=Nintendo_-_Nintendo_3DS_(Digital)_(Updates_and_DLC)_(Encrypted)_MIA) — **high confidence** that some digital/update/DLC material is separated at dataset level; not evidence of a universal entry field.
- [Redump redumper CLI dumping guide](https://wiki.redump.org/index.php?title=Dumping_Guide_%28redumper_CLI%29) — **high confidence** for the upstream category vocabulary and structured submission fields.
- [Redump search parameters](https://wiki.redump.org/index.php?title=Redump_Search_Parameters) — **medium/high confidence** that ordinary DAT downloads do not expose the full upstream record vocabulary.
- [Redump scope](https://wiki.redump.org/index.php?title=Redump.org) and [IBM PC moderation guidance](https://wiki.redump.org/index.php?title=Moderating_guidelines_for_IBM_PC_and_other_systems) — **high confidence** for accepted non-retail/bonus/compilation/covermount cases and naming/category practice.
- [TOSEC project structure and scope](https://www.tosecdev.org/the-project/what-is-tosec/30-about-tosec/the-project) — **high confidence** for branches and catalogued content categories.
- [TOSEC Naming Convention](https://www.tosecdev.org/tosec-naming-convention) — **high confidence** for delimited demo, development, country/language and media fields.
- [OMNI_DAT browser](https://omni-games.info/omnidat.php) — **high confidence** that DAT-level Group/Ecosystem/Type/Platform fields are displayed; **low confidence** that a stable machine-readable schema can currently be imported because none was documented in the reviewed public material.
- [OMNI Games Info GitHub organization](https://github.com/omni-games-info) — **medium confidence** for the public distinction between OMNI_DAT technical metadata and OMNI_TITLES; it does not publish the core schema.
- [MAME software-list guidelines](https://docs.mamedev.org/contributing/softlist.html) and [MAME command-line/listxml documentation](https://docs.mamedev.org/commandline/commandline-all.html) — **high confidence** for base XML/software-list structure and its limits.
- [Libretro database repository](https://github.com/libretro/libretro-database) and [Libretro database documentation](https://docs.libretro.com/guides/databases/) — **high confidence** for system databases and metadata DAT sidecars; **low confidence** for direct use in EmuWiz until an adapter and reliable join are designed.

Repository conclusions are based on the current `main` worktree at the research
date. This document intentionally proposes no code, schema migration, branch or
pull request.
