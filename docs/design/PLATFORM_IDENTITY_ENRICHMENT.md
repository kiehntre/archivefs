# Platform identity enrichment

Platform identity enrichment is a metadata-only layer. It does not organise a
ROM library and cannot rename, move, copy, delete, create, or link ROM files.

## Reused architecture

- `platform::PLATFORMS` remains the only canonical platform and alias registry.
- RomM slugs are accepted only after the existing strict RomM normaliser maps
  them to a canonical registry ID.
- DAT evidence is accepted only from the existing audit outcome when the local
  file has a cryptographic `Exact` or `Exact (multiple)` verdict and the DAT
  source is assigned to an exact canonical platform ID.
- Successful enrichment uses the existing `platform_assignments` history and
  its manual-assignment precedence. No database migration is added.
- Provider worker generations are carried by evidence and compared with the
  current game/library generation before persistence.

## Resolution

The resolver is pure and deterministic. Evidence is sorted before resolution,
so provider arrival order cannot affect the result.

1. A manual assignment wins and remains user-selected.
2. Verified DAT and usable canonical RomM evidence are considered together.
   Agreement resolves once and retains both provenance records. Disagreement is
   a conflict with no selected provider platform.
3. Existing strong game/emulator identity is used when no authoritative
   provider evidence exists.
4. Lower-confidence inference is used next.
5. Otherwise the result remains Unknown.

Stale evidence from a different generation is ignored. Unknown RomM values,
substring near-matches, non-exact DAT verdicts, stale RomM records, and RomM
records with matching conflicts never become platform evidence.

## Persistence and conflicts

Resolved RomM, verified DAT, and agreeing DAT+RomM results are persisted through
the existing assignment sources `romm`, `verified_dat`, and
`verified_dat+romm`. The existing manual assignment remains stronger than all
three. A conflict retires an earlier provider-enrichment winner so timing cannot
leave either provider silently active.

The full typed conflict (including both evidence records) is session-level. The
current database schema has no conflict-row shape, and adding a second identity
store or a migration solely for conflict display would be an unrelated
architectural expansion. A later persistence change can serialize the existing
typed resolution without changing its precedence rules.

## Future canonical library organisation

Organisation is intentionally out of scope. The RomM identity cache exposes the
provider slug for a canonical platform from the imported platform mapping, for
example canonical `PSP` to the instance's `psp` slug. It never derives a folder
name from a display label. A future organiser can consume that mapping without
duplicating the platform table or weakening identity resolution.
