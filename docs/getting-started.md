# Getting started

This guide walks through the shortest path from nothing to a working
`cachegirl` setup: installation, one manager, one namespace, one file, an
explicit cleanup pass and a clean shutdown. It assumes stable Rust and a
project with a `main.rs`.

## Installation

Add the crate to `Cargo.toml`:

```toml
[dependencies]
cachegirl = { version = "0.1" }
```

The default feature set enables the `filesystem` backend, which is what
this guide uses. `memory`, `cache`, `async`, `serde`, `metrics` and
`tracing` are opt-in; see [configuration](configuration.md) for the full
feature matrix.

## The smallest complete program

Every snippet in this guide is a complete runnable program:

```rust
use cachegirl::TempManager;

fn main() -> cachegirl::Result<()> {
    // pick a root of your own; the library never imposes one
    let dir = std::env::temp_dir().join(format!("cachegirl-guide-{}", std::process::id()));

    let manager = TempManager::builder()
        .root(&dir)
        .create_if_missing(true)   // create the root if it does not exist
        .cleanup_on_shutdown(true) // sweep expired resources on shutdown
        .build()?;

    // resources live in namespaces
    let scratch = manager.namespace("scratch")?;

    // files are written through the path the handle gives you
    let file = scratch.create_file("packet-dump.bin")?;
    std::fs::write(file.path(), b"hello cachegirl").expect("file is writable");

    // the registry tracks size at creation; re-stat to sync it after writes
    let tracked = scratch.refresh_size(file.id())?;
    println!("tracked {tracked} bytes");

    // explicit release when the application is done with the resource
    scratch.release(file.id())?;

    // an explicit cleanup pass; the library never spawns background threads
    let report = manager.cleanup_expired()?;
    println!("cleanup: {report}");

    // orderly shutdown: sweep (if configured) + lock file removal
    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
```

What just happened, step by step:

1. `TempManager::builder().root(...)` confines everything the manager
   creates to one directory. The root is canonicalized; nothing the
   manager creates ever escapes it.
2. `manager.namespace("scratch")` creates (or returns) the logical
   isolation boundary. All resources belong to exactly one namespace.
3. `scratch.create_file(...)` materializes a real file at
   `root/<instance>/<namespace>/<name>` and registers it.
4. `scratch.refresh_size(...)` syncs the tracked size with
   `fs::metadata`, which is what cleanup and quota accounting read.
5. `scratch.release(...)` destroys the file through the filesystem
   backend and unregisters it.
6. `manager.cleanup_expired()` runs one pass over the registry: it
   expires what has outlived its TTL, enforces quotas, and returns a
   report.
7. `manager.shutdown()` blocks new namespaces, optionally sweeps, and
   removes this instance's lock file.

## Where files live

With `root = /var/tmp/myapp` and `instance_id = "instance-abc"`, a file
created in namespace `scratch` as `packet-dump.bin` lands at:

```text
/var/tmp/myapp/instance-abc/scratch/packet-dump.bin
```

The instance id defaults to a random value and is stable for the lifetime
of the process. You choose it explicitly with `.instance_id("...")` if
you want predictable paths, or if an external recovery tool must be able
to attribute a directory to a run.

## Choosing an instance id

Pass a stable id when:

- you run the same binary many times and want lock files and directories
  to be attributable in one glance,
- an external janitor needs to recognize a crashed instance's directory,
- you embed the manager in a long-lived service and want a stable name in
  logs and metrics.

A random id is the right default for short-lived processes where nothing
else cares about the directory name. Ids must match
`[A-Za-z0-9._-]+`, must not be `.`/`..`, and must not end with `.` or
conflict with reserved device names (`NUL`, `CON`, ...); the builder
rejects anything else with `ConfigError::InvalidInstanceId`.

## Next steps

- [configuration.md](configuration.md) — every builder knob, defaults, when to change them.
- [namespaces.md](namespaces.md) — quotas, metadata, per-namespace sweeps.
- [ownership-and-leases.md](ownership-and-leases.md) — who destroys what, and when.
- [cleanup-policies.md](cleanup-policies.md) — TTL, eviction, custom policies.
