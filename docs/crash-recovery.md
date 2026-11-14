# Crash recovery

`cachegirl` instances write a small lock file under their instance
directory. After an unclean shutdown, the next start can reclaim stale
instance directories without touching live ones. This page explains the
mechanism, the exact decision table, and what to do when something
looks wrong.

## What an instance directory looks like

With `root = /var/tmp/myapp` and `instance_id = "instance-abc"`:

```text
/var/tmp/myapp/
  instance-abc/
    .cachegirl.lock     <- this instance's lock, removed on clean shutdown
    scratch/
      packet-dump.bin
```

Every instance gets its own directory. Namespaces are subdirectories of
it. The lock file is plain text — one `key=value` per line, `#` starts
a comment, unknown keys are ignored:

```text
# cachegirl instance lock — do not edit
version=2
pid=1234
created=1763100000
instance=instance-abc
start=windows:2026-11-14T10:14:09.9156628Z
```

| Key        | Meaning                                             | Required |
|------------|-----------------------------------------------------|----------|
| `version`  | Format version (`2`); ignored by current readers.   | no       |
| `pid`      | Owning process id.                                  | yes      |
| `created`  | Creation time, seconds since the Unix epoch.        | no       |
| `instance` | Instance id; must match the directory name.         | no       |
| `start`    | Opaque process-lifetime token (see below).          | no       |

The lock is written directly (not via rename) on startup after the instance
directory is confined to the canonical root; a torn read is treated as a
corrupt lock, which readers are required to tolerate. `shutdown()`
removes the lock file as its last step; a crash never removes it.

The lifetime token (`start`) is opaque and only comparable against a
token produced by the same code on the same machine:

- Linux: `linux:<btime>:<starttime>` from `/proc/stat` and
  `/proc/<pid>/stat` (field 22),
- Windows: the process start time via `tasklist` + PowerShell
  `Get-Process().StartTime`,
- other Unix: `kill -0` plus `ps` for the start time.

Its purpose is to detect **PID reuse**: pid 1234 dying and being reused
by an unrelated process must not make a stale directory look live, and
a stale pid being recycled must not make a live directory look dead to
us — either way the token comparison protects the decision.

## When is an instance considered stale?

For each directory directly inside the root (top-level files are
ignored, and the current instance's directory is never touched):

| lock? | condition                                                        | verdict |
|-------|------------------------------------------------------------------|---------|
| no    | directory mtime older than `stale_instance_after`                | stale   |
| no    | directory mtime newer than `stale_instance_after`, or unavailable/future | skipped |
| yes   | unparsable, oversized (> 4096 bytes), non-UTF-8, missing pid     | skipped (corrupt; never deletes) |
| yes   | `instance` field does not match the directory name               | skipped (misplaced; never deletes) |
| yes   | pid is not running                                               | stale   |
| yes   | pid is running, recorded token differs from probed token         | stale (pid was reused) |
| yes   | pid is running, token matches (or no token recorded)             | kept    |
| yes   | liveness probe cannot tell                                       | skipped (never deletes on doubt) |

If `recovery_policy` is `ReportOnly`, stale candidates are classified
but nothing is removed. `OnStartup` removes. `Never` does nothing at
all and produces an empty report.

## How a started manager uses it

Recovery runs inside `build()` when `cleanup_on_start(true)`; it never
aborts the build. Problems are collected into the report you can read
with `manager.recovery_report()`, and a `RecoveryCompleted` event is
emitted to the sink. `recovery_policy` decides how aggressive that pass
is:

- `OnStartup` (default with `cleanup_on_start`) — reclaim what is
  certainly stale.
- `ReportOnly` — diagnose only, good for a first deployment against a
  populated root.
- `Never` — skip recovery entirely.

```rust
use cachegirl::{RecoveryAction, RecoveryManager, RecoveryPolicy};

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!("cachegirl-recovery-{}", std::process::id()));

    // plant a fake stale instance: a directory with a lock whose pid
    // is far beyond any realistic pid on this machine
    let dead = root.join("instance-crashed");
    std::fs::create_dir_all(&dead).expect("fake instance dir");
    std::fs::write(
        dead.join(".cachegirl.lock"),
        format!("version=2\npid={}\ncreated=1763100000\ninstance=instance-crashed\n", 0x7FFF_FFFEu32),
    )
    .expect("fake lock");
    std::fs::write(dead.join("data.bin"), vec![b'x'; 256]).expect("fake payload");

    // diagnose first
    let recovery = RecoveryManager::new(&root);
    let report = recovery.recover(RecoveryPolicy::ReportOnly, "instance-current")?;
    println!("report-only: {report}");
    assert!(dead.exists());

    // then reclaim
    let report = recovery.recover(RecoveryPolicy::OnStartup, "instance-current")?;
    for entry in &report.entries {
        println!("  {} -> {:?}: {}", entry.instance, entry.action, entry.reason);
    }
    assert!(report.entries.iter().any(|e| e.action == RecoveryAction::Removed));
    assert!(!dead.exists());

    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
```

This snippet deliberately does not use `cleanup_on_start` on a live
manager for the demonstration; it exercises `RecoveryManager` directly,
which is the same code path.

## Guarantees and non-guarantees

Recovery **never** touches:

- the current instance's directory, even if its lock looks stale,
- directories that fail any verification step (corrupt lock, unknown
  probe result, name mismatch),
- files at the top level of the root (only directories are candidates),
- directories behind symlinks (symlinks to directories are skipped as
  candidates, and every candidate is re-validated to sit directly
  inside the canonical root immediately before removal).

Recovery **does** delete:

- directories whose lock names a pid that is not running,
- directories whose lock names a live pid whose lifetime token does not
  match (PID reuse),
- directories without a lock older than `stale_instance_after` (default
  1 hour) — in `OnStartup` only.

It is best-effort: one failing instance never stops the pass; failures
are collected in `failure_reasons` and the pass continues. An
unreadable root is a hard error (`RecoveryError`), which the manager
turns into a failed report entry instead of a build failure.

## PID reuse, and why the token exists

PIDs are recycled. On a long-lived machine, pid 1234 dying today and a
new process getting 1234 tomorrow are ordinary events. A lock recording
pid 1234 with no token cannot distinguish "the crashed instance is
still running" from "an unrelated process now holds that pid"; treating
every live pid as alive would leak directories forever. The lifetime
token closes that hole: it identifies one *lifetime* of a process, so
a recycled pid produces a token mismatch and the directory is reclaimed.
Legacy locks without a token fall back to liveness-only, which is the
conservative answer: keep the directory.

## What to do in an incident

1. **You are not sure the root is clean.** Build with
   `cleanup_on_start(true).recovery_policy(RecoveryPolicy::ReportOnly)`,
   read `manager.recovery_report()`, and inspect the entries. Nothing
   is deleted.
2. **You see a directory you do not recognize.** Leave it. If its lock
   is corrupt it will be reported and skipped forever; a human decides.
3. **You see `Failed` entries.** One directory could not be reclaimed
   (permissions, a file held open on Windows, removal racing another
   process). Other instances were still processed. Fix the cause or
   remove it manually; the next pass retries.
4. **You need a different staleness threshold.**
   `.stale_instance_after(Duration::from_secs(...))` on the builder or
   `RecoveryManager::with_stale_after` when driving recovery directly.
   Lower it only for test roots or scratch deployments.
5. **You want zero recovery.** `recovery_policy(RecoveryPolicy::Never)`
   or just leave `cleanup_on_start(false)` (the default). The instance
   lock is still written and removed; only the startup pass is skipped.
6. **Two instances share one root.** That is the designed case; each
   keeps its own directory and lock. There is no cross-instance
   coordination beyond the filesystem, and no global state.

## Lock file lifecycle

The lock is removed by `shutdown()` as a best-effort final step
(removal failure does not fail shutdown). A clean process exit therefore
never leaves a stale lock, and any lock found in a directory whose pid
is dead is evidence of a crash, by construction. If your process exits
the way it is supposed to but an operator sees a lock file left behind,
that is a bug worth reporting.
