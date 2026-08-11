# Encrypted GameCube/Wii Action Replay Decryption — Licensing Research

Status: research only. No code was changed, no branch/commit/PR was created.

Scope: determine whether EmuWiz (MIT licensed) can legally and safely implement
support for encrypted GameCube/Wii Action Replay (`XXXX-XXXX-XXXXX`, a
base-32-style alphabet with typo-tolerant aliases) codes while remaining MIT
licensed.

> **Note on the referenced prior report.** The task referenced
> `docs/research/ENCRYPTED_ACTION_REPLAY_RESEARCH.md` as existing research. That
> file was **not present** in the repository, in any branch, or in any worktree
> at the time of this research (only `CHEATS_MODS_EXPANSION_RESEARCH.md` and
> `DAT_GAMES_ONLY_FILTER_RESEARCH.md` exist under `docs/research/`). This
> report therefore proceeds from primary sources and from the technical
> context already documented in `docs/research/CHEATS_MODS_EXPANSION_RESEARCH.md`
> (§4, §8, §11) and in `bsfree_gamecube.rs`'s doc comments, and it independently
> verifies the licensing conclusions.

> **Not legal advice.** This is engineering/licensing research. Verified facts
> are labelled `[FACT — verified]`; reasoned conclusions are labelled
> `[INTERPRETATION]`. Nothing here should be relied on as formal legal advice.

---

## 1. Executive conclusion

**YELLOW — a potential path exists, but permission/provenance must be resolved
before an MIT-native implementation.**

- The cipher is a **DES-variant** (reverse-engineered from Datel's GameCube
  Action Replay device). Its building blocks — the eight `table0-7` DES
  SP-boxes, the CRC-16/KERMIT tables, the base-32 alphabet, and the DES-style
  Feistel/permutation structure — are **standard, public-domain functional
  technology**, available independently of any GPL source `[FACT — verified]`.
- The **algorithm/idea** is not copyrightable under US law (`17 U.S.C. §102(b)`:
  ideas, procedures, processes, systems, and methods of operation are not
  protected) `[INTERPRETATION]`.
- The **specific source-code expression** is copyrighted: Dolphin's
  `ARDecrypt.cpp`/`ARDecrypt.h` are `GPL-2.0-or-later` and explicitly derive
  from **GCNcrypt** (`Copyright (C) 2003-2004 Parasyte`) `[FACT — verified]`.
  Copying that expression into an MIT codebase would be GPL-derived and
  incompatible with MIT. **Do not copy the GPL implementation.**
- The **genuinely opaque part** is a small set of scheme-specific functional
  constants (`gentable0-3`, `gensubtable`, and the exact seed-derivation and
  round/permutation arrangement). Their **only documented authoritative sources
  are the GPL implementations and Parasyte's unlicensed freeware**, and no
  permissively-licensed or independent specification of them was found
  `[FACT — verified]`. Whether these are copyrightable expression or merely
  functional data is **uncertain and jurisdiction-dependent**
  `[INTERPRETATION]` — which is the reason this is YELLOW, not GREEN.
- A defensible path exists: obtain permission/provenance from the author
  (Parasyte) or independently derive the scheme constants from published test
  vectors, then write a **fresh** implementation from a specification —
  i.e., a clean-room or re-implementation, never a port of the GPL code.
- Safer interim alternatives: keep encrypted AR **browse-only** (current
  state, zero risk), or invoke a **separate GPL helper executable** (EmuWiz
  stays MIT; the helper is GPL and process-separate).

---

## 2. Dolphin provenance

Verified from the primary source:

- `Source/Core/Core/ARDecrypt.cpp` and `ARDecrypt.h` carry:
  `// Copyright 2008 Dolphin Emulator Project` and
  `// SPDX-License-Identifier: GPL-2.0-or-later` `[FACT — verified]`
  (fetched from `dolphin-emu/dolphin@master`).
- The file header states verbatim:
  `// Most of the code in this file is from: GCNcrypt - GameCube AR Crypto
  Program // Copyright (C) 2003-2004 Parasyte` `[FACT — verified]`.
- Dolphin uses this decryption at runtime in `ActionReplay.cpp`
  (`DecryptARCode`), in the Qt cheat-code editor, and in the Android
  `ARCheat.cpp` — all GPL-2.0-or-later `[FACT — verified]`.

Consequence: **Dolphin's AR decryption is GPL-2.0-or-later and is a port of
GCNcrypt.** Any verbatim or close-derivative reuse in EmuWiz would make EmuWiz
a GPL-derived work (incompatible with the project's MIT licence) `[INTERPRETATION]`.

---

## 3. GCNcrypt provenance

- **Author:** "Parasyte" (Jay), a well-known game-hacker and co-author of the
  Dolphin "GameCube Action Replay Code Types" documentation. Confirmed in his
  own words (GameHacking.org Q&A, 17 April 2009): *"Within the first few days
  after I released GCNcrypt 1.0 (a program capable of encrypting and decrypting
  GameCube Action Replay codes)..."* `[FACT — verified]`.
- **Dates:** the program is dated `Copyright (C) 2003-2004` in the Dolphin and
  omniconvert headers `[FACT — verified]`.
- **Distribution/licence:** GCNcrypt was distributed as a Windows **freeware**
  tool; community mirrors (e.g. Project Pokemon "GCN Crypt") host the binary.
  **No open-source licence statement** was found on the original distribution,
  and Parasyte's Q&A makes no licence grant `[FACT — verified — absence of
  evidence]`. It predates and does not use an OSI licence `[INTERPRETATION]`.
- **Source circulation:** GCNcrypt's source circulated in the game-hacking
  community and was imported into Dolphin (2008, under GPL) and into
  Pyriel's `omniconvert` (`source/armax.c`, header:
  `Copyright (C) 2003-2004 Parasyte; Copyright (C) 2008 Pyriel`, licensed
  `GPL version 2 or later`, with the note *"Parasyte is the source for the bulk
  of the code in this file"*) `[FACT — verified]`.

Consequence: GCNcrypt's code has no confirmed permissive licence; its only
documented licence is as GPL through Dolphin/omniconvert `[INTERPRETATION]`.

---

## 4. Other implementations and specifications found

### 4.1 Implementations found (source-level)

| Implementation | Licence | Relationship to GCNcrypt |
|---|---|---|
| Dolphin `ARDecrypt.cpp`/`.h` | GPL-2.0-or-later | Direct port; header credits GCNcrypt |
| `pyriell/omniconvert` `armax.c` | GPL-2.0 | Credits Parasyte as the source of the bulk of the file; PS2 ARMax variant (different `gensubtable`) |
| GCNcrypt original (freeware) | None / uncertain | The root source |

A repository-wide code search (`grep.app`) for the distinctive symbols
`GCNcrypt`, `genseeds`, `Unscramble1`, and `DecryptARCode` returned **only
Dolphin and omniconvert** as hits `[FACT — verified]`. `crates.io` has no
GameCube Action Replay decrypt crate `[FACT — verified]`.

**No MIT/BSD/Apache/zlib or public-domain implementation of the GameCube/Wii
AR decrypt was found** `[FACT — verified — absence of evidence]`.

### 4.2 Specifications / documentation found

- **Dolphin wiki — "GameCube Action Replay Code Types"** (by kenobi &
  Parasyte): documents the **decoded** code-type semantics (address/value
  bit-fields, zero codes, conditionals, memory-copy, master codes). It does
  **not** document the encryption/scrambling scheme or its constants
  `[FACT — verified]`.
- **gc-forever thread "Action Replay decoder - pseudocode"** (t=5112):
  provides concrete **encrypted → decrypted test vectors** (e.g. the SDload
  boot codes `7YPR-RKZZ-MH6W5 …` → `0F420000 88000000 …`), sourced from
  gc-linux.org `[FACT — verified]`. This is independent test data.
- **gc-linux.org SDload wiki**: publishes encrypted AR codes used to boot
  homebrew — usable as independent vectors `[FACT — verified]` (referenced in
  the thread).
- **No independent formal specification of the cipher itself** (the
  base-32-style text → binary mapping, the DES-variant scrambling, the seed
  derivation, or the constants) was found outside source code
  `[FACT — verified — absence of evidence]`.

### 4.3 Patents

No patent on the Datel GameCube AR encryption scheme was found in this
research `[FACT — verified — absence of evidence]`. This is **not** a patent
clearance opinion; a proper clearance search is out of scope `[INTERPRETATION]`.

---

## 5. Algorithm vs implementation copyright analysis

US copyright protects **expression**, not **ideas** (`17 U.S.C. §102(b)`).
The relevant separations:

| Layer | Copyright status | Assessment |
|---|---|---|
| **Algorithm / idea** (a DES-variant cipher with a base-32 encoding, as reverse-engineered from a hardware device) | Not copyrightable as an idea/process | A functional scheme is a fact about how the device works. Reverse-engineering a device's functional behaviour is lawful, and the result is unprotected facts. `[INTERPRETATION]` |
| **Source-code expression** (GCNcrypt's C; Dolphin's C++) | Copyrightable; GPL-2.0-or-later (Dolphin) | Copying or closely adapting it into MIT is GPL contamination. **Avoid entirely.** `[INTERPRETATION]` |
| **Constants / tables** | See §6 — functional-data question | Uncertain; treat as the primary risk. `[INTERPRETATION]` |
| **Test vectors** (encrypted ↔ decrypted pairs) | Facts; not copyrightable | Published independently (gc-forever, gc-linux). Fine to use for validation. `[INTERPRETATION]` |
| **Documentation/specification** (code-type semantics) | Copyrightable text, but documents decoded semantics, not the cipher | The cipher itself lacks an independent spec. `[INTERPRETATION]` |

The core tension: the **idea** is freely usable, but the only complete,
authoritative **expression** of the scheme-specific details currently sits in
GPL or unlicensed code. EmuWiz needs the scheme's constants and exact
structure from an independent, non-GPL origin to be safe `[INTERPRETATION]`.

---

## 6. Fixed-table / constants analysis

The decrypt needs the following fixed data. Each is assessed separately:

| Constant | What it is | Independent non-GPL source? | Copyrightability |
|---|---|---|---|
| `table0-7` (8 × 64 × u32) | The **standard DES SP-boxes** (`DES_SPtrans`) | **Yes** — these exact values are the canonical DES S-box→P-box combination tables published in FIPS PUB 46–based DES and reproduced in countless public-domain, MIT, and Apache-2.0 implementations (e.g. OpenSSL `des_local.h`/`D_ENCRYPT` uses the identical 8-table, `& 0x3f`, 2/10/18/26-bit indexing). | Public-domain functional data from a government standard. Low risk. `[INTERPRETATION]` |
| `crctable0-1` (2 × 16 × u16) | Nibble-wise **CRC-16/KERMIT** tables | Yes — standard CRC-16/KERMIT lookup tables, public/standard functional data. | Low risk. `[INTERPRETATION]` |
| `filter` alphabet | Base-32-ish alphabet `0123456789ABCDEFGHJKMNPQRTUVWXYZILOS` with I→1, L→1, O→0, S→5 | Partially — a base-32 alphabet is a common technique; the **specific** set and the I/L/O/S remapping are scheme-specific. No independent source found. | Functional encoding table; likely unprotected, but no non-GPL provenance confirmed. `[INTERPRETATION]` |
| `gentable0-3`, `gensubtable` (≈ 0x38+8+0x10+0x30+8 bytes) | Scheme-specific **seed-derivation tables**; `gensubtable` (8 bytes) is effectively the key-schedule constant. The **GC value differs from the PS2 ARMax value**, confirming it is per-console functional data. | **No** independent non-GPL source found. Only Dolphin (GPL), omniconvert (GPL), and GCNcrypt (freeware, no licence). | Functional data, but provenance is the open question. This is the main contamination point. `[INTERPRETATION]` |
| `genseeds` | 48 × u32 seed array **derived deterministically** from `gentable0-3` + `gensubtable` | It is *computed*, so an implementation can recompute it from the scheme constants — no table copying needed if the scheme constants are legitimately sourced. | Derived data; follows the status of its inputs. `[INTERPRETATION]` |
| Round/permutation structure (8 Feistel rounds; `Unscramble1/2` bit-swap permutations) | Functional algorithm structure, DES-like | The permutation masks (`0xF0F0F0F0`, `0xFFFF0000`, `0x33333333`, `0x00FF00FF`, `0xAAAAAAAA`) and the DES round structure are standard DES building blocks | Functional procedure; low risk. `[INTERPRETATION]` |

**Net assessment:** the DES SP-boxes, CRC tables, and the DES-like structure
are public/standard functional data — the "secret sauce" is the **scheme
constants** (`gentables` + `gensubtable`) and their exact use. Under US law
these are arguably **unprotected functional data** (merger doctrine: there is
effectively one required set of numbers for the scheme to interoperate with
the hardware; and facts about a device's behaviour are not copyrightable
subject matter). However, this is **an interpretation, not settled law**, and a
conservative project should not copy these numbers from GPL code without
resolving provenance `[INTERPRETATION]`.

---

## 7. Clean-room feasibility

A clean-room implementation is **realistic and would be the strongest
defensible path**, for these reasons:

1. **Most of the algorithm is standard public technology.** DES is fully
   specified by FIPS PUB 46; the SP-boxes and CRC tables are public. A
   spec-driven implementer needs no GPL access for these.
2. **The scheme is small and well-pinned by test vectors.** Independently
   published vectors (gc-forever t=5112, gc-linux SDload codes) fix the
   encrypted→decrypted mapping. Combined with knowledge that the cipher is a
   DES variant, a researcher can **reverse-engineer the scheme constants from
   the test vectors** (cryptanalytic derivation) rather than from GPL code.
   Because `genseeds` is *derived* from the scheme constants by a deterministic
   algorithm, a specification can describe the derivation, and the vectors
   verify the outputs — so Researcher B never needs the raw GPL tables.

**Defensible process (for when implementation is authorised):**
- **Researcher A** (may study the GPL implementation) writes an
  implementation-independent specification: the text→binary mapping, the
  DES-variant round structure, and the seed-derivation **as derived from the
  published test vectors** (not copied verbatim from GPL code), and records the
  provenance of every constant (DES SP-boxes → FIPS; CRC → standard; scheme
  constants → derived-from-vectors).
- **Researcher B** (has not viewed GPL source) implements solely from the
  specification and from public DES references, and validates against the
  independent test vectors and against Dolphin's runtime behaviour used
  strictly as a black-box oracle.

**Does clean-room genuinely help?** Yes for the *copying-expression* risk: it
keeps all GPL-derived code out of the MIT tree, which is the decisive issue.
It does **not** by itself remove the *underlying-rights* uncertainty about the
scheme constants (see §6) — if Parasyte or Datel holds protectable rights in
those specific constants, clean-room derivation from vectors is a
lawful-independent source, but the safer route is still to **ask the author**
(§9). Clean-room is meaningful and recommended if implementation proceeds
`[INTERPRETATION]`.

---

## 8. External-helper alternative

| Approach | EmuWiz licence impact | Practicality / notes |
|---|---|---|
| **Invoke an existing GPL program as a separate executable** (subprocess) | EmuWiz (MIT) is a separate work that merely *invokes* the GPL program; process separation avoids creating a derivative work. EmuWiz stays MIT. | **No existing GPL program exposes GC/Wii AR decryption as a headless CLI.** Dolphin decrypts internally at runtime only; there is no `dolphin --decrypt-ar` interface. Not practical today. `[FACT — verified — no such CLI found]` |
| **Ship/build a dedicated small GPL helper** (a minimal Dolphin/GCNcrypt-derived CLI) and invoke it | Same process-separation principle; the helper is GPL and distributed separately (or built by the user). EmuWiz stays MIT. | Feasible and common pattern (like calling `ffmpeg`/`git`), but operationally heavy: packaging, the helper's own GPL compliance (source offer), and a dependency on an extra binary. Requires that the helper itself be legally distributable (it is — GPL allows GPL tools). `[INTERPRETATION]` |
| **Dynamically locate Dolphin / another installed tool** | Same as invoking a GPL program. | No decrypt CLI exists to locate. Impractical today. `[FACT — verified]` |
| **Keep encrypted AR browse-only** | Zero licence risk. | Current EmuWiz state. Loses the feature. |
| **Find / commission an independently licensed implementation** | MIT-compatible if written fresh from the spec/vectors. | None exists publicly; would need clean-room or permission (see §7, §9). |

The external-helper route is the **safest way to ship the feature today
without any MIT-licence change**, at the cost of deployment complexity
`[INTERPRETATION]`.

---

## 9. Relicensing / permission option

- **The right person to ask is the author: "Parasyte" (Jay)** — reached via
  GameHacking.org (he is a staff/alumnus), the Kodewerx community, or the
  Dolphin forums. He authored GCNcrypt and co-authored the Dolphin AR
  documentation, and he is on record advocating an *"open source revolution"*
  for game hacking — making him likely receptive `[FACT — verified]`.
- What to ask: (a) whether the algorithm/constants have an independently
  licensed source or specification; (b) whether he would grant permission for
  an MIT implementation of the scheme (he is the only identified author of the
  reverse-engineered scheme constants); (c) whether he knows of a permissive
  implementation.
- A permission grant from Parasyte would resolve the **scheme-constants**
  provenance concern for the reverse-engineered scheme. It does not, by
  itself, clear any rights Datel might hold in the *device's* scheme — though
  no patent or other claim was found, and the scheme is reverse-engineered
  functional behaviour `[INTERPRETATION]`.
- Do **not** rely on Dolphin's GPL headers as an implied permission for MIT
  use: Dolphin relicensed the file GPL and the upstream source was never
  MIT.

---

## 10. Recommended EmuWiz decision

**YELLOW.** Recommended course, in order:

1. **Interim (safe now):** keep encrypted GameCube/Wii AR **browse-only** —
   current behaviour, no change, zero risk.
2. **First step (low effort, high value):** ask Parasyte for provenance and
   permission (§12). This is the single highest-leverage action.
3. **If permission is granted, or an independent source for the scheme
   constants is confirmed:** implement **fresh** from a specification
   (clean-room, §7), sourcing the DES SP-boxes and CRC tables from
   public-domain standards, and validate against the independent test vectors.
   Do **not** copy the GPL implementation.
4. **If permission is not obtainable and no independent source exists:**
   ship the feature only via a **separate GPL helper executable** (§8), or
   keep it browse-only. Do not embed GPL-derived code in the MIT codebase.

---

## 11. Exact conditions required before an MIT-native implementation

1. **Provenance resolved** for the scheme-specific constants
   (`gentable0-3`, `gensubtable`, the base-32 alphabet, and the exact
   round/permutation arrangement) — either (a) written permission from the
   author (Parasyte), or (b) an independently documented source, or
   (c) derivation from the published test vectors by a documented method.
2. **No GPL code in the tree:** the implementation is written from a
   specification (clean-room / re-implementation), not ported from Dolphin or
   GCNcrypt/omniconvert.
3. **Public standards for the public parts:** DES SP-boxes from FIPS/Public
   Domain sources; CRC-16/KERMIT from standard tables.
4. **Validation against independent vectors** (gc-forever t=5112 / gc-linux
   SDload), and optionally against Dolphin's runtime behaviour as a
   black-box oracle.
5. **Provenance recorded in the repository** (a note mirroring this report),
   so future maintainers can verify the independence.
6. **Scope discipline** unchanged from the P0 work: only codes whose
   decryption then classifies into the already-trusted hex-pair subset become
   installable; identity gating and transaction safety are reused unchanged.

---

## 12. Draft upstream enquiry (Parasyte)

Subject: GCNcrypt / GameCube Action Replay decrypt — licence & provenance

> Hi Jay/Parasyte,
>
> I'm working on an MIT-licensed emulator-adjacent tool (EmuWiz) and I'm
> researching the licensing of the encrypted GameCube/Wii Action Replay
> (base-32-style `XXXX-XXXX-XXXXX`) decryption scheme. The only implementations I
> have found are GPL (Dolphin's `ARDecrypt.cpp`, which credits your GCNcrypt)
> and your original freeware GCNcrypt, so I'd like to understand the
> provenance before writing a fresh, MIT-native implementation.
>
> Three questions:
> 1. Is there an independent, permissively licensed source or written
>    specification for the algorithm and its fixed tables/constants
>    (the seed/`gensubtable` constants, the base-32 alphabet, and the
>    DES-variant round structure)?
> 2. If not, would you be willing to grant permission for the
>    reverse-engineered scheme constants and algorithm to be implemented
>    under the MIT licence (with attribution)?
> 3. Do you know of a permissively licensed implementation of the scheme we
>    could reference or adopt?
>
> If it's easier, I'm also happy to discuss on GameHacking.org or the
> Kodewerx forums.
>
> Thanks for your time — and for the original work.

(Do **not** send this without project/legal sign-off; it is a draft.)

---

## 13. Sources

**Primary sources (fetched and read):**
- Dolphin `Source/Core/Core/ARDecrypt.cpp` and `ARDecrypt.h`
  (`dolphin-emu/dolphin@master`): GPL-2.0-or-later headers; "Most of the code
  in this file is from: GCNcrypt - GameCube AR Crypto Program, Copyright (C)
  2003-2004 Parasyte".
- Dolphin `Source/Core/Core/ActionReplay.cpp` (GPL-2.0-or-later; `DecryptARCode`
  usage; `DeserializeLine` recognises the `XXXX-XXXX-XXXXX` form).
- `pyriell/omniconvert` `source/armax.c` and `armax.h` (GPL-2.0; credits
  Parasyte as the bulk source; PS2 ARMax variant with a different
  `gensubtable`).
- OpenSSL `crypto/des/des_local.h` (Apache-2.0): `D_ENCRYPT` uses the identical
  8×64 `DES_SPtrans` structure and indexing as the AR cipher's `table0-7`.
- Parasyte Q&A, GameHacking.org (17 April 2009; archived via the Wayback
  Machine): authorship of GCNcrypt 1.0; pro-open-source stance.
- Dolphin wiki, "GameCube Action Replay Code Types" (by kenobi & Parasyte):
  decoded code-type semantics only; no cipher documentation.
- gc-forever forum, "Action Replay decoder - pseudocode" (t=5112): independent
  encrypted→decrypted test vectors referencing gc-linux.org SDload.
- `grep.app` repository-wide code search: `GCNcrypt`, `genseeds`,
  `Unscramble1`, `DecryptARCode` — only Dolphin and omniconvert hit.
- crates.io API search: no GameCube Action Replay decrypt crate.

**Verified facts vs interpretation:** facts are labelled in-line above.
Copyright analysis (§5–§6) and the clean-room/helper/recommendation sections
are engineering interpretation, not legal advice.

---

**Verdict: YELLOW.**

Potential path exists (DES-based structure is public; the author is known and
likely approachable; clean-room derivation from independent test vectors is
feasible), but permission/provenance for the scheme-specific constants must be
resolved first, and the implementation must be written fresh — never copied
from the GPL implementations. Interim browse-only or an external GPL helper
are the zero-risk options.
