# RetroArch cheat cache locking

ArchiveFS coordinates every process using one RetroArch cheat-source cache
root with a single exclusive operating-system advisory lock. The lock covers
trusted-source listing and inspection, offline selection, retrieval and
publication, snapshot inventory and verification, pinning, prune planning,
confirmed pruning, and abandoned-staging planning and cleanup.

## Lock identity and filesystem behaviour

On Unix, ArchiveFS opens the exact cache-root directory and applies `flock`
to that directory descriptor. It does not derive identity from a UTF-8 or
lossy display string, create a PID file, create or delete stale lock files, or
modify immutable snapshot contents. Absolute non-filesystem-root paths are
required; traversal and symlink components are refused. Distinct byte paths,
including non-UTF-8 and prefix-collision paths, therefore lock independently.

A missing cache root remains an empty read-only result for inventory and
inspection and is not created merely to lock it. Retrieval creates its cache
root through the existing safe-directory workflow only when network retrieval
would already require a writable cache, then locks it before inspecting,
staging, downloading, or publishing anything.
If another process creates a previously missing root after a read-only command
starts, that command retains its initial empty/missing view rather than begin
unlocked reads.

Platforms without the required advisory-lock implementation fail with
`cache_lock_unsupported`; ArchiveFS never silently continues unlocked. Locking
semantics ultimately depend on the mounted filesystem honoring `flock`.

## Timeout and lifetime

Acquisition uses nonblocking attempts separated by 25 ms sleeps, with a
five-second production timeout. Timeout is deterministic
(`cache_lock_timeout`), returns a non-zero CLI status, and is represented by a
versioned structured error in JSON mode. There is no indefinite wait or busy
spin.

The guard owns the open directory descriptor. Normal return, error unwinding,
or process termination closes the descriptor and releases the lock; there is
no poison or stale-owner recovery step.

## Ordering and revalidation

The cache lock is the outermost and only cache-wide lock. Public operations
acquire it exactly once and pass a locked context to internal helpers. Helpers
must never reacquire it, because recursive acquisition is intentionally not
supported and deterministically times out.

Retrieval holds the lock through cache selection and, when needed, the full
download, validation, extraction, and atomic publication operation. This
simple policy favors proof and consistency over concurrency.

Prune planning releases its lock when the plan is returned. Confirmed
execution reacquires the same root lock and retains all existing per-candidate
identity, pin, current-pointer, hash, path, and symlink revalidation. Locking
is additional defense and does not replace immediate revalidation.

## Security boundary

The protocol coordinates cooperating ArchiveFS processes. As with the cache
itself, the filesystem owner can manually rename, replace, or delete cache
directories outside ArchiveFS. Remote filesystems that do not implement
advisory locks consistently are unsupported for safe concurrent operation.
