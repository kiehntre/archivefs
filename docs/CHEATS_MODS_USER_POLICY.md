# Cheats & Mods: user policy

This is a short, plain statement of how ArchiveFS handles cheats and mods.
For the fuller technical design behind it, see
[`docs/CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md).

## You're in charge

ArchiveFS treats you as a capable adult who can decide what to do with your
own files. It doesn't second-guess your choices or lock things away "for
your own good." What it does do is give you real information before you
act, and refuse a small number of things that are concrete technical
hazards rather than matters of personal choice.

## Safe by default

Nothing installs, applies, or modifies anything until you explicitly confirm
it. Retrieving a cheat catalogue never installs a cheat. Discovering a
RetroArch profile never changes your RetroArch configuration. Your original
archive and source files are never rewritten, sanitized, or deleted as a
side effect of any of this.

## Local inspection, nothing uploaded

Where ArchiveFS checks content for structural safety, that check runs on
your machine. Filenames, file contents, hashes, scan results, and metadata
about your files and library are never sent to ArchiveFS's developers or
anyone else. The only network traffic this involves is downloading a
reviewed, built-in cheat catalogue - nothing about your local content goes
out over that connection.

## No automatic execution of unknown code

ArchiveFS may inspect a file. It does not run one. Executables, scripts,
installers, and macros inside anything you retrieve or import are never
launched automatically, at any stage - not during preview, not during
installation, not during rollback.

## Trusted, Unverified, and Blocked

Every cheat or mod source ArchiveFS can represent falls into one of three
states:

- **Trusted** - a source ArchiveFS's developers have reviewed: known
  origin, known format, and (where applicable) enforced download limits
  and integrity checking.
- **Unverified** - ArchiveFS hasn't reviewed this source. Community
  content, something a friend sent you, or your own local files all start
  here.
- **Blocked** - a concrete technical problem, not a judgment call: an
  unsafe file path, a symlink or special file where one shouldn't be, a
  malformed archive, something that breaks a resource limit, or content
  ArchiveFS simply cannot inspect safely.

**Unverified does not mean malicious.** It means "not reviewed yet." Most
community content is unverified and perfectly fine. Passing a structural
check also doesn't reclassify something as trusted - it means the check
found nothing wrong, which is a narrower claim.

## What ArchiveFS is not

ArchiveFS is not antivirus software. Its structural checks look for a
specific, bounded set of hazards - unsafe paths, malformed archives,
unexpected executables where a format shouldn't contain one, and similar
issues. That is real protection, but it is not a general malware scanner,
and it will never be marketed or treated as one.

If a future version lets you turn local safety checking off, turning it off
will not make unsafe files safe - it will only stop ArchiveFS checking them.
That trade-off will always be shown to you plainly before you make it, and
we'll ask you to confirm it.

## Your responsibility

You're responsible for having the right to use, modify, and share whatever
cheats, patches, mods, or other files you bring into ArchiveFS. ArchiveFS
doesn't verify ownership or licensing, and it won't block a structurally
safe file just because its licensing is unclear or absent.

ArchiveFS must not be used to bypass copy protection, licensing systems, or
other technical protection measures.

## Respect for the people who make these games

Developers, artists, musicians, writers, testers, and publishers put real
work into the games this software helps you organize and enjoy. Supporting
legitimate releases is part of what keeps future games, updates, and
preservation efforts possible. Cheats and mods can be a great way to enjoy
games differently - use them in a way that respects that work.
