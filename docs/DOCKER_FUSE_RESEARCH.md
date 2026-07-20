# Docker and FUSE deployment research

Date accessed: **2026-07-20** (Europe/London).

Status: research and design only. This document does not define a supported
image, production Compose file, host helper, API, or CLI contract.

## Executive summary

Decypharr and AltMount converge on the same basic Linux container mechanism:
run the application service as a configured UID/GID, expose `/dev/fuse`, add
`SYS_ADMIN`, disable the container AppArmor profile, enable FUSE
`allow_other`, and bind a host mount tree into the producer container with
recursive shared propagation. Neither primary Compose example uses full
`privileged` mode. Both use `restart: unless-stopped`. [D1][D2][A1][A2]

That convergence is useful evidence, but it is not evidence that the design is
safe or complete. `SYS_ADMIN` is broad, `apparmor:unconfined` removes an
important defence, `allow_other` broadens access to the mounted files, and a
writable recursively shared host bind expands the impact of a compromised
container. Docker also requires the host bind's backing mount to support the
requested propagation; `:rshared` on a Compose line does not create a shared
peer group from an unsuitable host mount. Bind propagation is Linux-only and
does not work for Docker Desktop. [K1][K2][K3]

AltMount supplies the strongest recovery pattern found: detach a stale mount
before touching or recreating its directory, validate responsiveness with a
bounded probe, retry recovery a bounded number of times, and force-detach the
kernel mount after recovery is exhausted so consumers do not accumulate on a
dead endpoint. It also shuts down in an explicit order and uses s6-overlay as
PID 1. These behaviours were strengthened after a v0.2.0 ghost-mount incident
that left consumers blocked in uninterruptible sleep. [A3][A4][A5][A6][I1]

Decypharr has similarly useful but less uniform patterns. Its embedded rclone
path retries mount creation, monitors every 30 seconds, attempts unmount and
remount on failure, and escalates through ordinary, lazy, and FUSE unmount
commands. Its custom DFS path pre-cleans stale mounts and has bounded mount and
unmount waits, but no equivalent periodic DFS responsiveness monitor was found.
Its Go process handles `SIGINT`/`SIGTERM` and stops the mount manager before
schedulers, queues, network pools, and storage. [D3][D4][D5][D6]

The recommended first supported ArchiveFS Docker release is **Mode A,
catalogue-only**. It needs no FUSE device, `SYS_ADMIN`, propagation, or
unconfined AppArmor and can establish image build, non-root UID/GID, persistent
SQLite, read-only source binds, health reporting, and backup semantics with a
much smaller risk surface. Mode B should follow only after an integration test
matrix proves host and sibling visibility, signal delivery, stale-mount
recovery, and permission behaviour on real Linux Docker Engine. Mode C (a
narrow host helper) is promising for eventual least privilege but is a new
security boundary and protocol, not a shortcut for the first release.

## Evidence rules and version boundary

- **Confirmed** means a cited implementation file, first-party documentation,
  image manifest, release, or issue directly supports the statement.
- **Inference** is explicitly labelled and is not represented as upstream
  behaviour.
- Decypharr findings use release tag `v2.4`, commit
  `fc6621e3bfc34075a9a1a800b905efa814776289` (2026-07-17).
- AltMount implementation findings use release `v0.3.2`, commit
  `14ad0f52656469a26aedb25412ab495fa91bfdec` (2026-07-18). Main was also
  inspected at `87bd53627571628cdc35dbd74b1bbb6aa3f0db09` (2026-07-19); no
  changes between that tag and main were found in the Docker, FUSE, rclone,
  shutdown, database, sample configuration, or primary README paths reviewed.
- Mutable image tags were inspected on 2026-07-20. Their digests are a snapshot,
  not a recommendation to deploy `latest`.

## Source table

| ID | Project/source | Exact path or page | Version/commit | Relevant evidence |
|---|---|---|---|---|
| D1 | [Decypharr repository](https://github.com/sirrobot01/decypharr) | [`Dockerfile`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/Dockerfile), [`scripts/entrypoint.sh`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/scripts/entrypoint.sh), [`.github/workflows/release.yml`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/.github/workflows/release.yml) | v2.4 / `fc6621e3` | Alpine multi-stage build, FUSE packages/config, healthcheck, PUID/PGID/UMASK, root-to-user transition, persistent `/app`, multi-registry multi-architecture publishing. |
| D2 | Decypharr | [`README.md`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/README.md), [`docs/.../installation.md`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/docs/src/content/docs/guides/installation.md) | v2.4 | `/mnt:rshared`, `/dev/fuse`, `SYS_ADMIN`, unconfined AppArmor, persistence and restart example. |
| D3 | Decypharr | [`main.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/main.go), [`cmd/decypharr/main.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/cmd/decypharr/main.go), [`pkg/manager/manager.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/manager/manager.go) | v2.4 | Signal handling, service cancellation, shutdown ordering, storage close. |
| D4 | Decypharr | [`pkg/mount/rclone/manager.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/mount/rclone/manager.go), [`client.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/mount/rclone/client.go), [`health.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/mount/rclone/health.go) | v2.4 | rclone RC lifecycle, retry, `AllowOther`, UID/GID/umask, cache, monitoring and forced unmount chain. |
| D5 | Decypharr | [`pkg/mount/dfs/manager.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/mount/dfs/manager.go), [`backend/hanwen/backend.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/pkg/mount/dfs/backend/hanwen/backend.go) | v2.4 | Custom FUSE startup timeout, pre-clean, readiness, `AllowOther`, graceful then lazy/forced unmount. |
| D6 | Decypharr | [`cmd/healthcheck/main.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/cmd/healthcheck/main.go), [`internal/config/mount.go`](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/internal/config/mount.go), [DFS docs](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/docs/src/content/docs/guides/mounting/dfs.md), [rclone docs](https://github.com/sirrobot01/decypharr/blob/fc6621e3bfc34075a9a1a800b905efa814776289/docs/src/content/docs/guides/mounting/rclone.md) | v2.4 | Health scope, mount/cache environment/config, documented permissions. |
| D7 | Docker Hub image | `docker.io/cy01/blackhole:latest` manifest | accessed 2026-07-20 | OCI index `sha256:d9df495fd156a379abd5b760ad54ed960881408599cd045c4d552a6d20bc7ce6`; linux/amd64 and linux/arm64 plus attestations. |
| I2 | Decypharr issue | [#122, symlink/mount path reports](https://github.com/sirrobot01/decypharr/issues/122) | opened 2025-08-22; open when accessed | Reports show correct path mapping across producer and consumers matters; one reporter resolved their case by correcting mount paths. Reports are observations, not proof of a general defect. |
| A1 | [AltMount repository](https://github.com/javi11/altmount) | [`docker/Dockerfile`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/docker/Dockerfile), [`docker/Dockerfile.ci`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/docker/Dockerfile.ci), [`docker/.../svc-altmount/run`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/docker/root/etc/s6-overlay/s6-rc.d/svc-altmount/run), [`.github/workflows/release.yml`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/.github/workflows/release.yml) | v0.3.2 / `14ad0f52` | Local and release build paths, LinuxServer Ubuntu base, s6 lifecycle, `abc` service user, FUSE/rclone, healthcheck, volumes and image publishing. |
| A2 | AltMount | [`docker-compose.yml`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/docker-compose.yml), [`docker-compose.volume-plugin.example`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/docker-compose.volume-plugin.example), [`README.md`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/README.md), [`config.sample.yaml`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/config.sample.yaml) | v0.3.2 | PUID/PGID, `/config`, `/mnt:rshared`, `/dev/fuse`, capability/security options, restart and cache/log settings; alternative rclone volume-plugin example. |
| A3 | AltMount | [`cmd/altmount/cmd/serve.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/cmd/altmount/cmd/serve.go), [`internal/api/fuse_handlers.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/api/fuse_handlers.go) | v0.3.2 | Signals, ordered shutdown, FUSE auto-start, responsiveness monitor, bounded recovery and final detach. |
| A4 | AltMount | [`internal/fuse/server.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/fuse/server.go), [`internal/fuse/backend/hanwen/backend.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/fuse/backend/hanwen/backend.go) | v0.3.2 | UID/GID, backend options, bounded stat probe, pre-clean and force-unmount fallback. |
| A5 | AltMount | [`internal/rclone/mount_service.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/rclone/mount_service.go), [`pkg/rclonecli/manager.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/pkg/rclonecli/manager.go), [`health.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/pkg/rclonecli/health.go) | v0.3.2 | Web-before-mount startup, rclone shutdown, liveness grace/failure threshold, subprocess restart and remount. |
| A6 | AltMount | [`internal/database/db.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/database/db.go), [`internal/slogutil/config.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/slogutil/config.go), [`internal/metadata/backup_worker.go`](https://github.com/javi11/altmount/blob/14ad0f52656469a26aedb25412ab495fa91bfdec/internal/metadata/backup_worker.go) | v0.3.2 | SQLite WAL/busy timeout/checkpoint loop, rotated stdout/file logs, scheduled bounded backups. |
| A7 | GHCR image | `ghcr.io/javi11/altmount:latest` manifest | accessed 2026-07-20 | OCI index `sha256:cd02c5ee4a840a6a3fef486846ab9e1e61d704d7a04def9e53783b29add2ba0b`; linux/amd64 and linux/arm64. |
| I1 | AltMount issue | [#539, ghost mount after recovery exhaustion](https://github.com/javi11/altmount/issues/539) | v0.2.0 report; closed 2026-04-27 | Primary incident report motivating wedged-rclone restart and final FUSE detach. Current code implements both mitigations; this does not prove all ghost-mount cases are eliminated. |
| K1 | Docker | [Bind mounts: configure bind propagation](https://docs.docker.com/engine/storage/bind-mounts/#configure-bind-propagation) | current docs, accessed 2026-07-20 | Default `rprivate`, meanings of shared/slave modes, Linux/bind-only limitation, Docker Desktop limitation and host prerequisite. |
| K2 | Docker | [Runtime privilege and Linux capabilities](https://docs.docker.com/engine/containers/run/#runtime-privilege-and-linux-capabilities) | current docs, accessed 2026-07-20 | Capability/privileged model and device exposure. |
| K3 | libfuse manual | [`mount.fuse3(8)`](https://man7.org/linux/man-pages/man8/mount.fuse3.8.html) | accessed 2026-07-20 | `allow_other`, `user_allow_other`, permission and unmount semantics. |
| K4 | Docker | [Protect the Docker daemon socket](https://docs.docker.com/engine/security/protect-access/) | current docs, accessed 2026-07-20 | Docker daemon control is a high-value host boundary; do not expose its socket to ArchiveFS. |
| R1 | ArchiveFS | [`docs/DATABASE_RECOVERY.md`](DATABASE_RECOVERY.md), [`docs/adr/0001-persistent-library-database.md`](adr/0001-persistent-library-database.md) | local `b921c6f3` | Current catalogue location, SQLite sidecar preservation, copy-first recovery, and catalogue-not-load-bearing boundary. |
| R2 | ArchiveFS | [`docs/library-views.md`](library-views.md), [`docs/security.md`](security.md), [`docs/architecture.md`](architecture.md) | local `b921c6f3` | Current Library View ownership rules, read-only source/mount safety, explicit actions, ratarmount backend and no-daemon boundary. |

## Decypharr findings

### Image and process model

Confirmed at v2.4:

- The build uses `tonistiigi/xx`, a Go 1.26 Alpine builder, CGO/FUSE headers,
  and a final `alpine:latest` stage. It downloads the current rclone binary at
  build time rather than pinning an rclone version. The final image installs
  `fuse3`, `su-exec`, `shadow`, certificates and timezone data, and appends
  `user_allow_other` to `/etc/fuse.conf`. [D1]
- The image declares `/app` as a volume, exposes port 8282, and runs a custom
  healthcheck every 10 seconds with 10 retries. The check exercises the qBittorrent
  API, web UI, and WebDAV endpoint; it does **not** test that a configured FUSE
  mount is responsive. [D1][D6]
- PID 1 is the shell entrypoint, which ends with `exec`. When initially root it
  creates or reuses the requested PUID/PGID, recursively changes ownership of
  `/app`, prepares `/app/logs` and `/app/cache`, and execs the application via
  `su-exec`. When the container is launched as non-root it skips ownership
  changes and directly execs the service. PUID, PGID and UMASK default to
  1000, 1000 and 022. [D1]
- Configuration, database/storage, rclone config, logs and the default cache
  live under the main config path (`/app` in the image). Alternate cache paths
  are configurable. The v2.4 installation page also shows separate `/cache`
  and `/downloads` binds in its `docker run` example, so the documentation is
  not a single canonical volume contract. [D1][D2][D4][D6]
- Container/runtime variables include PUID, PGID, UMASK and LOG_PATH; the
  application also reads `ENABLE_PPROF` and supports configuration overrides
  under a `DECYPHARR_` prefix, with double underscores for nested fields. This
  is confirmed behaviour, not a proposed ArchiveFS environment contract.
  [D1][D3][D6]
- The README/Compose example uses `cy01/blackhole:latest`, while the Docker run
  documentation names `sirrobot01/decypharr:latest`. The inspected public
  `cy01/blackhole:latest` index is multi-architecture. The release workflow
  confirms that `ghcr.io/sirrobot01/decypharr` and
  `docker.io/cy01/blackhole` are both release outputs. The unqualified
  `sirrobot01/decypharr:latest` in the Docker-run documentation does not match
  either fully specified workflow destination. [D1][D2][D7]

### FUSE, propagation and permissions

Confirmed at v2.4:

- The primary Compose example binds `/mnt` to `/mnt:rshared`, maps `/dev/fuse`
  read/write, has `SYS_ADMIN` as its only `cap_add` entry, sets
  `apparmor:unconfined`, and does not set `privileged: true` or override
  seccomp. Docker's normal default capability set is separate from `cap_add`.
  [D2][K2]
- Both custom DFS and embedded rclone request `AllowOther`. The DFS backend also
  requests `default_permissions`; the rclone path accepts configured UID, GID
  and umask. The image enables non-root use of `allow_other` in `fuse.conf`.
  [D1][D4][D5]
- The project can mount its custom DFS, an embedded rclone WebDAV mount, an
  external rclone RC service, or no mount. Cache location and limits are
  configurable for DFS and rclone. rclone output is captured to a rotated log
  (`10 MB`, 15 days, five backups); application logging defaults under
  `/app/logs`. [D4][D6]
- **Conditional result, not an upstream guarantee:** mounts created below the
  container's `/mnt` bind can appear on the host only if the corresponding host
  mount belongs to a shared peer group. Sibling containers can see them only if
  they bind the corresponding host tree with propagation that receives those
  submounts (normally `rslave` for read-only consumers, or `rshared` only when
  bidirectionality is genuinely required). [D2][K1]

### Startup, recovery and shutdown

Confirmed at v2.4:

- The rclone mount creates its destination, retries a failed mount three times
  after the initial attempt, and force-unmounts a known failed mount before a
  new attempt. Its fallback sequence is ordinary `umount`, lazy `umount -l`,
  `fusermount -uz`, then `fusermount3 -uz`, bounded by ten seconds. [D4]
- Embedded rclone is checked every 30 seconds. A failed mount health operation
  marks the in-memory mount unhealthy, unmounts, waits one second and remounts.
  The recovery code does not contain AltMount's current consecutive-failure
  threshold or explicit restart of a wedged rclone subprocess. [D4][A5]
- Custom DFS creates the mountpoint, runs the same stale-unmount chain before
  mounting, waits for mount creation/readiness, and cleans up a late/orphaned
  mount result after timeout. Shutdown asks the Go FUSE server to unmount,
  closes its VFS, waits, then escalates to system unmount tools within bounded
  contexts. No periodic DFS responsiveness monitor was found. [D5]
- The main process catches `SIGINT` and `SIGTERM`. On shutdown it cancels
  services, waits for service goroutines, stops the mount manager first, then
  schedulers, work queue, Usenet connections, repair work and storage. The HTTP
  server also calls graceful shutdown. `SIGKILL` cannot invoke this path. [D3]
- Compose assumes Docker's `unless-stopped` restart policy. There is no
  `stop_grace_period` in the primary example, so Docker's configured/default
  grace period—not an upstream-tested value—is relied upon. [D2]

## AltMount findings

### Image and process model

Confirmed at v0.3.2:

- The self-contained Dockerfile builds the frontend with Bun 1 and builds a
  static CGO Go binary with Go 1.26 Alpine. The release workflow instead
  prebuilds the frontend and selects `docker/Dockerfile.ci`, whose Go builder
  is 1.26 Bookworm; both use `lscr.io/linuxserver/baseimage-ubuntu:jammy` at
  runtime. They install Docker CLI, FUSE3, rclone downloaded as `current`, and
  enable `user_allow_other`. [A1]
- LinuxServer's s6-overlay remains PID 1. The service run script uses
  `s6-setuidgid abc`, so initialization is root but the AltMount application
  runs as the LinuxServer `abc` user whose IDs are controlled by PUID/PGID.
  FUSE-presented ownership is also derived from those variables. [A1][A4]
- The image declares `/config` and `/metadata` volumes. The sample database,
  metadata, backups, logs, rclone configuration and default caches are under
  `/config` unless explicitly placed elsewhere. The primary Compose file binds
  `/config` and `/mnt`; the README additionally documents `/metadata` as
  optional. [A1][A2][A6]
- Container/runtime variables shown by the image and primary example include
  PUID, PGID, PORT, HOST, JWT_SECRET and COOKIE_DOMAIN. The direct FUSE backend
  also accepts `ALTMOUNT_FUSE_BACKEND`; most operational settings live in
  `/config/config.yaml`. This is confirmed behaviour, not a proposed ArchiveFS
  environment contract. [A1][A2][A4]
- Its healthcheck polls `/live` every five seconds with a ten-second timeout,
  five-second start period and three retries. `/live` is HTTP liveness only;
  mount health is separately monitored inside the application and does not
  directly determine Docker health. [A1][A3]
- The inspected `ghcr.io/javi11/altmount:latest` image index supports
  linux/amd64 and linux/arm64. [A7]

### FUSE, propagation and permissions

Confirmed at v0.3.2:

- The primary Compose file binds `/mnt:/mnt:rshared`, maps `/dev/fuse`, adds
  `SYS_ADMIN`, disables AppArmor confinement, uses `restart: unless-stopped`,
  and does not request full privileged mode or a seccomp override. [A2]
- The direct FUSE backend defaults to `AllowOther`, reports configured
  PUID/PGID, disables xattrs/security labels, uses direct mount and bounded
  entry/attribute cache settings. Its rclone mode separately supports
  `allow_other`, UID/GID, umask, read-only mode, cache limits and timeouts.
  The sample currently defaults the rclone mount to writable and umask 002;
  ArchiveFS should not inherit those media-workflow defaults. [A2][A4][A5]
- Host/sibling visibility has the same prerequisite as Decypharr: the producer
  bind alone is insufficient if the host tree is not shared. No sibling
  consumer service is present in AltMount's primary Compose file. [A2][K1]
- A separate AltMount example delegates mounting to a host-installed rclone
  Docker volume plugin. In that example the AltMount service itself has no
  `/dev/fuse`, `SYS_ADMIN` or propagated `/mnt` bind; the external plugin owns
  the mount mechanics. This is an alternative deployment, not proof of a
  narrowly scoped or safer helper, and it still exposes the Docker socket for
  AltMount's updater. [A2]
- The Compose example binds `/var/run/docker.sock` and adds the host Docker
  group for the auto-update feature. This is unrelated to FUSE and grants an
  application a powerful Docker daemon control channel. ArchiveFS should reject
  this pattern entirely. [A1][A2][K4]

### Startup, recovery and shutdown

Confirmed at v0.3.2:

- HTTP starts first; AltMount polls `/live`, then starts its rclone service and
  auto-starts direct FUSE when configured. The direct backend force-unmounts
  the target before mounting. API and auto-start paths also issue lazy FUSE and
  ordinary lazy unmount before even calling `stat`/`mkdir`, specifically to
  avoid blocking on a stale endpoint. [A3][A4][A5]
- Direct FUSE readiness is explicit. A monitor validates the mount every 15
  seconds; each validation permits only one outstanding `stat` and returns
  after five seconds. Recovery force-unmounts, waits two seconds and creates a
  fresh server. After three failed recovery attempts, it force-detaches and
  marks the mount failed instead of leaving a ghost mount. [A3][A4]
- This logic directly addresses issue #539's v0.2.0 incident, in which a wedged
  rclone RC process caused repeated recovery calls to time out while the kernel
  mount remained and consumers accumulated in D-state. Current rclone health
  requires three consecutive failed probes, honours a 90-second startup grace,
  restarts a wedged rclone subprocess, and remounts known mounts. [I1][A5]
- Shutdown handles `SIGINT`, `SIGTERM`, `SIGQUIT` and `SIGHUP`, cancels the root
  context, stops the direct-FUSE manager, health workers, queue workers,
  metadata backup worker and rclone mount service, then gives the HTTP server a
  30-second shutdown context. rclone unmounts known mounts in parallel (30
  second bound), interrupts its subprocess, kills after ten seconds if needed,
  and removes only empty mount directories. [A3][A5]
- SQLite uses WAL, `synchronous=NORMAL`, a 30-second busy timeout, bounded
  connection pool and periodic checkpoints. Logs go to stdout and a rotated
  JSON file. Metadata backups are scheduled and retention-bounded. These are
  useful operational patterns, but ArchiveFS's existing copy-first database
  recovery policy remains authoritative for its own catalogue. [A6]

## Side-by-side comparison

| Concern | Decypharr v2.4 | AltMount v0.3.2 | Confidence |
|---|---|---|---|
| Runtime base | `alpine:latest` [D1] | LinuxServer Ubuntu Jammy [A1] | Confirmed |
| Build | multi-stage Go/CGO with `xx`; rclone-current [D1] | Bun frontend + static Go/CGO; rclone-current [A1] | Confirmed |
| PID 1 / service user | shell entrypoint `exec`; root setup then PUID:PGID via `su-exec`, or direct non-root [D1] | s6-overlay; service as `abc` after root init [A1] | Confirmed |
| Persistent roots | `/app`; configurable external cache [D1][D6] | `/config`, `/metadata`; configurable cache/backups/logs [A1][A2] | Confirmed |
| FUSE permissions | `/dev/fuse`, `SYS_ADMIN`, AppArmor unconfined, not full privileged [D2] | same [A2] | Confirmed |
| Seccomp | no override in example [D2] | no override in example [A2] | Confirmed absence from cited examples only |
| Propagation | producer `/mnt:rshared` [D2] | producer `/mnt:rshared` [A2] | Confirmed; host/sibling result conditional [K1] |
| `allow_other` | enabled in image and both mount paths [D1][D4][D5] | enabled in image; configurable backend option defaults true [A1][A2][A4] | Confirmed |
| Host-visible | possible with correctly shared host backing mount [D2][K1] | same [A2][K1] | Conditional inference from Docker semantics |
| Sibling-visible | sibling must bind same host tree and receive propagation [K1][I2] | same; not in primary example [A2][K1] | Conditional inference |
| Restart | `unless-stopped` [D2] | `unless-stopped` [A2] | Confirmed |
| Docker health | API/UI/WebDAV, not mount [D6] | `/live`, not mount [A1][A3] | Confirmed |
| Graceful shutdown | SIGINT/TERM; mount first, storage last [D3] | s6 + SIGINT/TERM/QUIT/HUP; ordered workers/mount/server [A1][A3] | Confirmed |
| Stale startup cleanup | DFS always; rclone for known failed in-memory state [D4][D5] | direct FUSE always; API avoids `stat` before detach [A3][A4] | Confirmed |
| Runtime recovery | rclone 30-second monitor and remount; no DFS monitor found [D4][D5] | direct FUSE 15-second monitor; bounded fresh-server recovery; rclone subprocess restart [A3][A5] | Confirmed / absence limited to reviewed paths |
| Logging | app log plus rotated rclone log [D1][D4] | stdout + rotated JSON file; separate rclone log [A5][A6] | Confirmed |
| Extra host authority | none in primary example [D2] | Docker socket for updater [A2] | Confirmed; reject for ArchiveFS |

## Security analysis

| Risk | What upstream does | ArchiveFS conclusion |
|---|---|---|
| Full privileged mode | Neither primary example requests it. [D2][A2] | **Reject.** It is broader than the demonstrated requirement. Absence of full privileged mode does not make the remaining configuration safe. |
| `SYS_ADMIN` | Both add it for mount operations. [D2][A2] | Treat Mode B as high privilege. Verify whether the exact ratarmount/FUSE path works with only `SYS_ADMIN` plus `/dev/fuse`; drop every other capability and set `no-new-privileges` if compatible. Research seccomp/AppArmor profiles instead of permanently disabling them. [K2] |
| `/dev/fuse` | Both expose it read/write. [D2][A2] | Expose only in Mode B and only to the mount-owning service. Never expose to catalogue-only or consumer containers. |
| `allow_other` | Both enable it; image config permits non-root use. [D1][D4][D5][A1][A4] | Needed when host/sibling users differ from the mounter, but it broadens access. Require explicit opt-in or make the access model unambiguous; pair with read-only FUSE output and controlled mode/UID/GID. [K3] |
| AppArmor/seccomp | Both examples use `apparmor:unconfined`; neither shows a seccomp override. [D2][A2] | Do not copy unconfined as an unexplained default. Test a narrow ArchiveFS profile. If unconfined remains necessary, document it as a major isolation loss. Keep Docker's default seccomp unless an exact syscall failure proves a narrowly reviewed exception is needed. |
| Writable host bind | Both producer examples bind a broad `/mnt` read-write. [D2][A2] | Bind only the dedicated mount root with propagation, not host `/mnt`. Bind archive sources separately as read-only. Make config/data/library-view roots narrowly scoped. |
| Writable sources | Upstream media workflows can write/cache through mounts; AltMount's sample rclone mode defaults writable. [A2][D6] | **Reject.** ArchiveFS archive sources stay read-only. A container compromise must not be able to rewrite original archives. |
| Recursive shared propagation | Both use `rshared`. [D2][A2] | Restrict bidirectional propagation to the producer's dedicated mount-root bind. Consumers should normally receive with `rslave`, limiting propagation back toward the host/producer. Validate host peer-group state before mounting. [K1] |
| Root versus non-root | Both do root initialization then run the app as mapped IDs; Decypharr also tolerates direct non-root. [D1][A1] | Adopt a non-root steady state. Rootful container + `SYS_ADMIN` still gives a stronger attack position than non-root. Confirm rootless Docker/FUSE separately; do not promise it from PUID/PGID alone. |
| Container escape impact | Neither upstream proves isolation safety. AltMount additionally exposes Docker socket in its example. [A2] | `SYS_ADMIN` + device + unconfined AppArmor materially weakens containment; this is not proof of an escape but raises impact. Never mount the Docker socket. Keep network/API exposure minimal and treat archives as hostile input. [K2][K4] |
| Symlinks / host paths | Decypharr issue #122 shows path parity and mapping mistakes are operationally common. [I2] | Library View roots must be dedicated and writable, while symlink targets must resolve meaningfully for intended consumers. Preserve ArchiveFS's existing manifest ownership and collision rules. |

Practical mitigations demonstrated by upstream include non-root service users,
UID/GID mapping, a narrowly named capability instead of full privileged mode,
health monitoring, bounded shutdown, stale-unmount escalation, log rotation and
persistent config/data. They mitigate particular failures; they do not cancel
the fundamental risk of `SYS_ADMIN`, FUSE, unconfined AppArmor, writable binds,
or (in AltMount's optional updater design) Docker socket access.
[D1][D2][D3][D4][D5][D6][A1][A2][A3][A4][A5][A6]

## ArchiveFS requirements mapping

| ArchiveFS need / upstream pattern | Decision | Reason |
|---|---|---|
| Scan separately bound source folders | **Adopt** | Bind each configured source read-only and preserve multi-source identity. This is narrower than either project's broad `/mnt` bind and matches ArchiveFS's existing source model. |
| Persistent SQLite catalogue | **Adopt** | Use one dedicated writable persistent data root. AltMount proves WAL/busy-timeout/checkpoint patterns are viable in a container, but ArchiveFS must preserve its own rollback-journal and copy-first recovery design rather than copy AltMount's settings blindly. [A6][R1] |
| Many independent archive mounts | **Adapt** | One producer can create nested FUSE submounts under a dedicated propagated root, as both projects do for mount trees. ArchiveFS must retain per-archive ownership, mount-state discovery and explicit user actions. [D2][A2] |
| Host-visible mounts | **Adapt** | Use the proven producer `rshared` concept only on a dedicated mount root and add a host prerequisite/diagnostic. The upstream Compose line alone is insufficient. [K1] |
| Emulator-container visibility | **Adapt** | Bind the same host mount root into consumers with `rslave` and normally read-only file access. Verify path parity and propagation; issue #122 shows mapping mistakes are common. [I2][K1] |
| Library View symlinks | **Adapt** | Give ArchiveFS a distinct writable Library View root and keep its manifest/collision controls. If consumers use different container paths, relative/host-path symlinks may not resolve; path mapping needs explicit validation. [R2] |
| Removable/unavailable sources | **Research further** | Neither upstream is a close analogue for independent removable read-only archives. ArchiveFS should mark a source unavailable without deleting catalogue evidence or auto-unmounting unrelated archives; current explicit retry policy should remain. |
| Graceful shutdown | **Adopt** | Direct signals to the real process, stop accepting new work, finish/cancel database work, unmount children, then exit. Both projects demonstrate ordered shutdown. [D3][A3] |
| Stale FUSE recovery | **Adopt** | AltMount's pre-stat detach, bounded responsiveness check, limited fresh-server retries, final detach and failed state are the strongest proven pattern. [A3][A4][I1] |
| Automatic remount after runtime failure | **Adapt** | AltMount demonstrates value, but ArchiveFS currently requires explicit mount/retry actions. Initial Mode B should auto-clean stale endpoints and report failure; automatic remount should remain opt-in/research until it cannot violate explicit-action semantics. |
| Non-root steady state | **Adopt** | Both projects map UID/GID and run the application service non-root. [D1][A1] |
| PUID/PGID compatibility | **Adapt** | Use host IDs for writable config/data/mount/view roots and FUSE presentation. Validate every root up front; never recursively chown archive sources. |
| Read-only archive sources | **Adopt** | Stronger than upstream media defaults and required by ArchiveFS's safety model. [R2] |
| Writable mount and Library View roots | **Adopt** | Separate them from sources and from each other. Only the mount root receives propagation. Library Views need ordinary write access, not FUSE capability. |
| Docker health versus mount health | **Adapt** | Upstream Docker checks are service liveness, not mount health. ArchiveFS needs separate liveness/readiness/mount-health states so restart policy does not loop on an intentionally unmounted catalogue-only service. [D6][A1][A3] |
| `restart: unless-stopped` | **Adapt** | Reasonable for a future long-running service, but ArchiveFS has no daemon today. Define the container process and persistence semantics before adopting a restart policy. |
| AltMount Docker socket updater | **Reject** | Unrelated to ArchiveFS and greatly expands host authority. [A2][K4] |
| Cheat/patch installation later | **Research further** | Reserve a writable journal/backup area now, but do not give the first container broad emulator-config writes. Future installers need destination-specific binds, preview/plan identity, backups and transaction journals. |

## Proposed deployment modes

These are capability and path models, not production-ready Compose syntax.

### Mode A: catalogue-only container

Purpose: scanning, health/catalogue operations, preview generation, Library View
planning, and future API/CLI work. Library View apply is available only when its
dedicated root is intentionally writable.

- No `/dev/fuse`, `SYS_ADMIN`, mount propagation, privileged mode, AppArmor
  exception, or Docker socket.
- Run the ArchiveFS process as the configured non-root UID/GID from process
  start where possible; use a narrow initialization step only if persistent
  directories need first-run ownership setup.
- Bind each source independently read-only. Persist configuration and
  catalogue data separately. Bind Library Views writable only when apply/repair
  is intended.
- Health distinguishes process liveness, config/database readiness, and
  source availability. A temporarily missing removable source is degraded, not
  an instruction to restart the container or delete catalogue rows.
- Suitable for native Docker Engine and Docker Desktop because it does not
  depend on Linux bind propagation; only Linux remains supported by ArchiveFS
  overall unless separately changed.

### Mode B: FUSE-enabled container

Purpose: Mode A plus ArchiveFS/ratarmount FUSE mounts visible on a Linux host
and optionally sibling emulator containers.

- Add only `/dev/fuse` and the minimum capability proven by integration tests;
  both researched projects use `SYS_ADMIN`. Never use full privileged mode.
  Start from Docker's default seccomp profile and research a narrow AppArmor
  policy before accepting `unconfined`. [D2][A2][K2]
- Bind one dedicated, empty host mount root to the identical container path
  with producer-side recursive shared propagation. Verify the host path is a
  shared mount before starting. Do not bind all of `/mnt`. [K1]
- Bind the same host root into emulator consumers using one-way recursive slave
  propagation; consumers do not receive `/dev/fuse` or capabilities. Whether
  the consumer bind can also be read-only without impairing submount visibility
  must be part of the integration matrix, not assumed.
- Force-detach stale children before inspecting/reusing them, then mount.
  Track each child independently; a bad archive must not destabilize the root.
  Use bounded readiness and responsiveness probes. On recovery exhaustion,
  detach and report a durable failed/retryable state. [A3][A4][I1]
- Handle `SIGTERM` at the application process, stop new mount work, unmount
  children with an overall bound, then close database/log resources. Document
  a container stop grace period derived from measured worst-case unmount tests.
- Security documentation must state that `SYS_ADMIN`, `/dev/fuse`, shared
  propagation and any unconfined LSM setting materially weaken isolation.

### Mode C: container plus host helper

Purpose: keep the main container unprivileged while a narrowly scoped local
host service performs only validated ArchiveFS mount lifecycle operations.

- Main container resembles Mode A and has no FUSE device/capability. The helper
  owns `/dev/fuse`, mount namespace interaction and the dedicated mount root.
- Communication is local-only over a root-owned Unix socket. Authenticate by
  peer credentials plus an application secret or mutually authenticated local
  channel; do not expose a TCP listener by default.
- Minimum operations: mount one validated source archive at one validated child
  of the configured mount root; unmount one owned child; list/health-check owned
  mounts. No arbitrary command execution, arbitrary paths, shell strings,
  recursive delete, ownership changes outside managed roots, Docker API access,
  or general filesystem browsing.
- Helper revalidates canonical source/mount paths and an explicit request/plan
  identity itself. It must not trust validation performed only in the
  container. Maintain an append-only ownership journal sufficient for reboot
  reconciliation without making SQLite load-bearing for safe unmount.
- Benefits: much smaller container privilege and no shared producer mount
  namespace. Costs: a privileged host package/service, authenticated protocol,
  version compatibility, installer/upgrader, audit log, OS-specific testing and
  a new high-value attack surface.

**Recommendation:** support Mode A first. Develop Mode B next as an explicitly
Linux-only, security-documented milestone after real propagation/recovery
tests. Keep Mode C as a design investigation until Mode B provides concrete
operations and invariants that can define a genuinely narrow helper protocol.

## Proposed ArchiveFS volume model

Paths below are conceptual container paths. They deliberately avoid production
Compose syntax and may change after implementation research.

| Purpose | Proposed container path | Access and lifetime | Propagation |
|---|---|---|---|
| Configuration | `/config` | Writable, persistent; may be mounted read-only for commands that never update config. Required. | None |
| Database/data | `/data` | Writable, persistent, required; SQLite main file and sidecars stay together. | None |
| Source archives | `/sources/<source-id>` | Read-only bind, persistent external data, one bind per configured source; source may be temporarily absent. | None |
| Mount root | `/mounts` | Writable bind, required only for Mode B; dedicated empty host root. | `rshared` producer; receiving propagation for consumers |
| Library Views | `/library-views` | Writable persistent bind when apply/repair is enabled; optional for read-only planning. | None |
| Logs | `/logs` | Optional writable persistent bind; otherwise stdout/stderr is authoritative and container runtime rotation applies. | None |
| Cache/temp | `/cache` and runtime-private temporary storage | Writable, optional and disposable; apply explicit size/free-space limits. Do not mix with SQLite or sources. | None |
| Backups and future cheat journals | `/backups` and `/journals` | Writable, persistent, optional initially; preferably separate failure domain from `/data`. No emulator destination bind until an installer design approves it. | None |

Configuration, data, mount and Library View roots must be non-overlapping.
ArchiveFS should validate owner/group/mode and free space without recursively
changing host content. Database backups must include SQLite sidecars according
to ArchiveFS's existing copy-first recovery rules; copying only the main file
is not a safe generic backup.

## Failure and recovery matrix

| Scenario | Expected ArchiveFS handling | Proven upstream approach |
|---|---|---|
| Container killed during mount | On next Mode B start, reconcile live kernel state before `stat` or directory creation; detach stale owned child, then leave failed/retryable unless the user requested remount. Never claim `SIGKILL` cleanup. | AltMount pre-stat lazy detach and fresh server [A3][A4]; Decypharr pre-mount cleanup [D4][D5]. |
| Host reboot | Persistent config/database survive; kernel mounts are gone. Reconcile catalogue against live mountinfo and report unmounted. Do not assume automatic remount unless explicitly configured. | Restart policy examples exist [D2][A2], but neither proves ArchiveFS-style per-archive reboot reconciliation. **Research further.** |
| Stale FUSE endpoint | Use a bounded probe with one in-flight check; avoid blocking filesystem calls before attempted detach; ordinary unmount then lazy/FUSE detach; mark failed after bounded recovery. | Strongly proven in current AltMount [A3][A4][I1]; command chain also in Decypharr [D4][D5]. |
| Source volume disappears | Preserve catalogue entry and mark source unavailable/mount degraded; do not delete source or unrelated mounts; retry only on explicit rescan/retry initially. | No close proven analogue. **ArchiveFS-specific research required.** |
| Destination root becomes read-only | Fail preflight/new mounts and Library View apply with exact path/error; keep sources/catalogue untouched; existing mounts get health-checked and safely detached if dead. | Both validate/create mount/cache paths and surface errors [D4][D5][A3][A5], but no complete read-only-root recovery was found. |
| Permission mismatch | Report effective UID/GID and exact failing root; no recursive chmod/chown of sources. Provide narrow ownership guidance for writable roots. | PUID/PGID/umask patterns in both [D1][D6][A1][A2]; path/permission operational reports in #122 [I2]. |
| Disk full | Stop catalogue/cache/view mutation cleanly, preserve SQLite error/sidecars, report affected filesystem and free space. Cache may be bounded/evicted by its own policy; never delete database evidence. | Both expose cache limits [D6][A2]; AltMount log rotation/backups and SQLite settings help operations [A6]. No complete disk-full proof. |
| SQLite database locked | Honour a bounded busy timeout for writers, identify active writer, and fail without deleting sidecars or auto-repair. Read-only diagnosis remains available where SQLite permits. | AltMount uses 30-second busy timeout/WAL [A6]. ArchiveFS's existing recovery policy is stricter and remains authoritative. |
| Mount propagation misconfigured | Refuse host-visible mode or clearly mark it degraded after checking host/container mountinfo; explain expected shared peer group and consumer mode. | Docker defines prerequisite/semantics [K1]. Neither project provides a complete startup diagnostic in reviewed paths. |
| `/dev/fuse` unavailable | Fail Mode B readiness before any mount; point to device/capability checks. Mode A remains healthy. | Both explicitly require device mapping [D2][A2]. |
| Container restarts while mounts remain | Detach/reconcile stale owned children before filesystem traversal, then honour explicit remount policy. | AltMount current startup cleanup [A3][A4]; Decypharr cleanup [D4][D5]. |
| Sibling cannot see mounts | Verify producer host path, host propagation, consumer bind source, receiving propagation and identical/resolvable paths. Do not add FUSE capability to consumer. | Docker propagation semantics [K1]; Decypharr #122 demonstrates real path-parity mistakes [I2]. |

## Recommended first Docker milestone

Deliver a **catalogue-only, non-root, Linux container contract** before any
Docker/FUSE implementation:

1. Define `/config`, `/data`, independent read-only sources, optional
   `/library-views`, `/logs`, `/cache`, `/backups` and `/journals`.
2. Make configuration/data paths container-configurable without weakening the
   current source/mount overlap checks or SQLite recovery model.
3. Define process liveness, catalogue readiness and degraded-source status as
   separate concepts.
4. Prove UID/GID behaviour and SQLite sidecar/backup handling under clean stop,
   `SIGTERM`, `SIGKILL`, restart, permission mismatch, lock contention and disk
   exhaustion.
5. Publish immutable version tags/digests and multi-architecture scope; do not
   download floating build dependencies without recording/pinning them.
6. Do not include `/dev/fuse`, `SYS_ADMIN`, propagation, Docker socket, broad
   host binds, a host helper, or an automatic updater in this milestone.

The next research gate for Mode B is an automated Linux Docker Engine test
matrix covering: host path initially private/shared; producer `rshared`;
consumer `rslave`; rootful and feasible rootless cases; AppArmor enforcing and
custom profiles; clean/forced stop; busy mount; stale endpoint; many independent
archives; removable source; and host reboot. Passing a happy-path mount test is
not sufficient.

## Unanswered questions

1. Does ratarmount's exact FUSE stack work inside the intended base image with
   only `/dev/fuse` + `SYS_ADMIN`, Docker default seccomp and a narrow AppArmor
   profile? Which syscalls/LSM rules are actually required?
2. Can ArchiveFS Mode B run as a non-root service user on all targeted Linux
   Docker Engine hosts, and what ownership/mode does ratarmount present to host
   and sibling users?
3. What host-side setup can be validated safely and persistently across reboot
   without shipping a broad root helper? Can documentation alone give reliable
   shared-peer-group setup on supported distributions/NAS platforms?
4. Does a consumer bind that is both read-only and `rslave` receive all nested
   ArchiveFS mounts on supported Docker versions/storage layouts?
5. How should explicit user-driven mount semantics interact with container
   restart: reconcile-only, restore a persisted desired state, or opt-in
   auto-remount?
6. What bounded mount-health probe avoids leaking blocked threads/processes for
   ratarmount endpoints? AltMount guards duplicate goroutines but a permanently
   blocked `stat` goroutine can still remain until detach. [A4]
7. What measured stop grace period is sufficient for hundreds or thousands of
   independent mounts, and how should failures be summarized when the overall
   deadline expires?
8. Should logs be stdout-only by default, with `/logs` purely optional, to avoid
   dual rotation and hidden disk use?
9. Can the existing fixed `~/.config` and `~/.local/share` paths be overridden
   cleanly for containers without changing native defaults or database safety?
10. How will Library View symlinks remain resolvable when source paths differ
    between host, ArchiveFS container and emulator containers?
11. Is Mode C worth its packaging/protocol/audit burden after a constrained
    Mode B is measured, and can its allowed-operation grammar be proven free of
    arbitrary-path or command execution?
12. Should ArchiveFS document multiple registries as Decypharr's workflow does,
    and how will it prevent an unqualified or mutable image reference from
    drifting away from the release workflow? [D1][D2]

## References

The source table is the citation index for this document. The most important
primary references are the pinned [Decypharr v2.4 tree](https://github.com/sirrobot01/decypharr/tree/fc6621e3bfc34075a9a1a800b905efa814776289),
the pinned [AltMount v0.3.2 tree](https://github.com/javi11/altmount/tree/14ad0f52656469a26aedb25412ab495fa91bfdec),
[AltMount issue #539](https://github.com/javi11/altmount/issues/539),
[Decypharr issue #122](https://github.com/sirrobot01/decypharr/issues/122),
[Docker bind propagation](https://docs.docker.com/engine/storage/bind-mounts/#configure-bind-propagation),
[Docker capability/device guidance](https://docs.docker.com/engine/containers/run/#runtime-privilege-and-linux-capabilities),
and the [libfuse mount manual](https://man7.org/linux/man-pages/man8/mount.fuse3.8.html).
