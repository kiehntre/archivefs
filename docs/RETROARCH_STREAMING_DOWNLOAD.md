# RetroArch streaming catalogue download

The Libretro catalogue ZIP is downloaded through ArchiveFS's HTTPS client; no
`curl`, `wget`, shell, or external process is used. Network access begins only
after the user confirms Download or Update in Sources.

## Corrected timeout semantics

The previous ureq configuration combined a 180-second global timeout with a
10-second `timeout_recv_response`. In ureq 3.3, the response deadline remains
in the timeout chain while the body reader runs. A large response could
therefore fail during a body read as `timeout: receive response`, long before
the configured global timeout. The transfer was chunk-copied already, but it
had no retry loop, no between-chunk cancellation, and calculated SHA-256 in a
second file pass.

The corrected bounds are:

- DNS/connect/TLS: 30 seconds
- idle time for each response-body read: 60 seconds
- revision resolution plus archive transfer: 15 minutes overall
- three total archive attempts
- five-second ordinary retry delay
- integer-seconds `Retry-After`, capped at 30 seconds

`timeout_recv_response` is deliberately unset because it is not a header-only
deadline in this ureq version. Response headers remain covered by the overall
deadline. Each request receives only the remaining operation time, while
`timeout_recv_body` supplies the per-read idle bound.

## Streaming and staging

The response body is read through one fixed 64 KiB buffer. Each chunk is
written to an exclusively created file inside a unique provider operation
directory, contributes immediately to SHA-256, is counted against the 256 MiB
compressed limit, and may emit rate-limited progress. `Content-Length` above
the limit is rejected before body reading; absent lengths are allowed; a
present length must parse and equal the received count. The completed staging
file is flushed and synced before digest and ZIP validation.

Retries never append or resume. A failed attempt file is removed and the next
attempt uses a new exclusive path. Operation cleanup covers interruption,
timeout, cancellation, size, digest, extraction, and verification failures.
While holding the provider/cache lock, a later operation may remove at most 16
crash-left directories whose numeric operation names, non-symlink directory
type, and location prove they are inactive. Unexpected staging entries are
left untouched and fail closed.

The active metadata pointer changes only after exact revision binding, archive
verification, bounded extraction, catalogue indexing, per-file manifest
verification, and atomic activation. Failed attempts cannot replace or hide a
previous valid snapshot.

## Retry and cancellation

Retries cover interrupted responses, idle and connection timeouts, connection
reset, temporary DNS failure, and HTTP 408, 429, 500, 502, 503, and 504. TLS
verification, HTTPS/host/redirect policy, authentication, declared or actual
size, digest, path, extraction, manifest, and schema failures are permanent.

Cancellation is checked before resolution/connect, after headers, between
chunks, before and during retry delay, before extraction, and before
activation. It returns typed `cancelled`, removes the current partial attempt,
and leaves the active snapshot usable.

## Progress and memory bounds

Sources shows resolving, connecting, downloading, retrying, archive digest
verification, extraction, file verification, activation, and cancellation.
Download events contain attempt, received bytes, optional declared total, and
bounded retry delay. Events are emitted at two-MiB intervals plus transitions,
with at most 512 callbacks per operation.

The retained response buffer is 64 KiB plus the HTTP/TLS parser's bounded
internal state and incremental SHA-256 state. The full ZIP is never held in
memory. The small revision JSON remains separately bounded at 64 KiB.

Existing limits remain 256 MiB compressed, 1 GiB expanded, 60,000 ZIP entries,
50,000 indexed catalogue files, 256 MiB per extracted file, 16 MiB manifest,
1,024 path bytes, 24 path components, 250:1 compression ratio, and three
redirects. One provider operation and one active staging file are allowed at a
time.

## Known limitations

Downloads restart from byte zero after failure; range resumption is not
implemented. HTTP-date `Retry-After` is not interpreted, so it falls back to
the bounded five-second delay. DNS preflight cannot pin the subsequent resolver
result, though exact host validation and TLS hostname verification remain in
force. Progress reflects compressed transfer bytes, not extraction progress by
individual file.

## Manual Saltbox QA

1. Open Sources with the current verified snapshot active.
2. Start Update.
3. Confirm visible byte progress.
4. Confirm the approximately 178 MiB catalogue completes.
5. Confirm exact revision and verification.
6. Confirm the previous snapshot remains until activation.
7. Disconnect networking mid-download.
8. Confirm retry state is visible.
9. Restore networking and confirm a later retry can succeed.
10. Cancel during download and confirm partial files are removed.
11. Confirm the old snapshot remains usable after cancellation.
12. Confirm Alien 3 matching remains available throughout failed updates.
13. Restart ArchiveFS and confirm no incomplete snapshot is active.
14. Confirm no curl, wget, or external process is launched.
