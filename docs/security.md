# Security

This page states the trust boundaries the library enforces, the ones it
does not, and the deployment posture that keeps the enforced ones
meaningful. It is written for operators and reviewers, not as a
vulnerability policy.

## The trust boundary is the root

Everything the manager creates lives under one directory: `root`. The
root is the confinement boundary. All filesystem operations the manager
performs are resolved against it, canonicalized, and checked to sit
inside it before any I/O happens:

- the root itself must exist (or be creatable) and is canonicalized at
  build time — the manager compares canonical paths everywhere;
- every candidate instance/resource path is canonicalized when it
  exists, and its deepest existing ancestor is canonicalized when it
  does not — a planted symlinked intermediate directory cannot steer a
  write or a delete outside the root;
- a path that canonicalizes outside the root is rejected with
  `AccessError::OutsideRoot`;
- a symlink whose canonical target escapes the root is rejected under
  the default `symlink_policy(Reject)` (or under `WithinRoot`);
- a planted symlink at `root/<instance_id>` cannot redirect the
  instance's lock file outside the root — the lock writer resolves and
  re-checks the confined directory before writing.

If an attacker can write anywhere under your root, the boundary still
holds for *that root* — but so does anything else you do with the same
filesystem. The library does not make an untrusted root safe; it makes
a trusted root tight.

## Name validation

Names are validated at the API boundary, before any path is built:

- resource names: non-empty, no leading/trailing whitespace, no `/`,
  `\`, `:` (NTFS alternate-data-stream separator), no control
  characters, no trailing `.` (Windows strips trailing dots, aliasing
  two names onto one file), no reserved device names
  (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, any
  extension, case-insensitive);
- namespace names: the same rules minus control characters and `:`
  (namespace names never become filenames directly, only directory
  names), plus rejection of `.`/`..` as standalone names;
- instance ids: `[A-Za-z0-9._-]+`, no `.`/`..`, no trailing `.`, no
  reserved device names.

Validation fails with typed errors (`AccessError::UnsafeName`,
`ConfigError::InvalidNamespaceName` / `InvalidInstanceId`) — never with
a silently sanitized name. If you build names from user input, validate
before you pass them in, or reject them; the library is the second
layer, not the first.

## Symlink policy

`SymlinkPolicy` controls the filesystem backend only:

- `Reject` (default) — any symlinked path component under the root is
  refused outright.
- `WithinRoot` — symlinks allowed, but only when the canonical target
  stays inside the canonical root. Dangling symlinks are refused.
- `Allow` — follow symlinks anywhere the process can reach.

None of these policies affects non-filesystem backends or custom
backends, which confine by their own contract.

## Creation never clobbers

Files are created `create_new(true)`: if the name is taken, creation
fails with `BackendError::Rejected` and nothing is written. Directories
are created with `create_dir`, same semantics. This protects against
both accidental overwrites of live data and a window where a crashed
run's lock directory aliases a new run's file.

## Lock files and recovery

Recovery deletes only after verification. It never deletes on:

- a corrupt, oversized (> 4 KiB) or non-UTF-8 lock,
- a lock whose `instance` field names a different directory,
- a live pid whose lifetime token does not match the lock (PID reuse),
- a probe result of "unknown" (permissions, missing `/proc`, ...),
- the current instance's directory, whatever its lock says.

A lock file can therefore be treated as an advisory claim owned by one
process; a stale lock is strong evidence of a crash, and a corrupt one
is strong evidence that a human should look. See
[crash-recovery.md](crash-recovery.md) for the full decision table.

## What the library does NOT protect against

- **TOCTOU on paths you pass in.** If you compute a path yourself and
  hand it to the application around the library's API, nothing
  canonicalizes your copy. Keep paths inside the library's abstractions
  (`file.path()` from a handle) and you inherit the confinement
  checks; reconstruct them yourself and you lose them.
- **An attacker inside the root.** Anyone who can write or rename files
  under the root is inside the trust boundary by definition. They can
  pre-create names the manager will later want to create (which fails
  closed), fill the disk, or race quota accounting by writing between
  `refresh_size` calls. Use a dedicated root.
- **Resource content.** The manager tracks size and metadata, not
  contents. It cannot tell valid from hostile bytes.
- **The machine's other processes.** `tasklist`/PowerShell/`ps` probes
  are best-effort; a process that hides its start time from the probe
  (or a sandbox that forbids probing) yields "unknown", and recovery
  keeps the directory. That is the intended conservative failure.
- **Denial of service through your own configuration.** `max_size` of
  `u64::MAX` is accepted and means nothing. Quotas work when you set
  them.
- **Signal safety.** Destructors run during unwind; a destructor doing
  I/O (owning drop) is legal but blocking. See
  [ownership-and-leases.md](ownership-and-leases.md).

## Filesystem permissions

On Unix, adopting an existing directory is ownership-checked before any
write: an existing `root` or `root/<instance_id>` owned by a different
uid fails the build with `ConfigError::DirNotOwned { dir, owner_uid,
current_uid }` instead of being adopted. Symlinks are judged by their
target — a link pointing at somebody else's directory is refused. The
owner is read through `MetadataExt::uid()` (no `unsafe`); the current
uid is probed via `/proc/self/status` on Linux and `id -u` on other
Unixes, and if the probe fails the check fails closed (a build that
cannot verify ownership refuses rather than adopts). Directories the
build creates are owned by the current user by construction and pass
without comment. On Windows there is no ownership primitive in the
standard library, so the check degrades to existence — existence is the
whole check there, and the same umask-style caveats apply. The gate is
refuse-before-create, not a race-proof sandbox: a hostile actor with
write access to the parent can still swap things between the check and
the first syscall.

The manager never sets, relaxes or audits permissions otherwise: files
and directories are created with the process's umask, lock files are
written with default permissions, and nothing is ever marked read-only.
If you need a root readable only by the service account, create it with
those permissions yourself and run the service as that account. On
multi-user machines, `root` must not be writable by untrusted users —
the confinement checks detect escapes, they do not prevent a co-located
writer from deleting or replacing your files within the root.

## Deployment recommendations

1. Give the manager a dedicated root on a filesystem with enough space
   and with the service's permissions; not a shared scratch tree, not a
   directory with other writers.
2. Set `max_size` and `max_resources` explicitly on multi-tenant or
   user-facing services. Unlimited defaults are a convenience, not a
   plan.
3. Default to `symlink_policy(Reject)`. Choose `WithinRoot` only if
   something legitimate inside the root must be a symlink.
4. Prefer `cleanup_on_start(true)` with `ReportOnly` on first deploys,
   then switch to `OnStartup` once the report looks boring.
5. Keep the cleanup scheduler inside your own process (or behind your
   own cron) — the library never spawns one, so a missing sweep is
   your signal from `pending_cleanup`/`resources` in statistics, not a
   supervision failure of the library.
6. If you log or export paths, remember they contain your instance id
   and namespace names; neither is secret, but neither is free of
   user-chosen data if you put any there.
