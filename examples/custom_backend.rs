//! Plugging your own storage engine in as a backend.
//!
//! Below is a complete, working backend: a small "blob store" that writes
//! every resource it materializes into its own subdirectory and keeps an
//! op log in memory. It implements the full [`ResourceBackend`] contract -
//! `name`, `create`, `release` - not a sketch of it.
//!
//! The lifecycle with a custom backend:
//!
//! 1. build it
//! 2. `manager.register_backend(...)` once
//! 3. `namespace.create_custom(...)` / `create_custom_with(...)` at the call site
//! 4. release flows back into the same backend that created the resource
//!
//! ```sh
//! cargo run --example custom_backend
//! ```

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cachegirl::backend::{ResourceBackend, ResourceRequest};
use cachegirl::{
    BackendError, CreateOptions, Event, ResourceCreationError, ResourceKind, ResourceMeta,
    TempManager, VecSink,
};

/// A backend that treats every resource as an opaque blob file.
///
/// Real enough to be useful - files land on disk under `root`, release
/// removes them - and small enough to read in one sitting. The in-memory
/// op log is here so the example can show exactly what the manager asked
/// the backend to do, and in which order.
struct BlobStoreBackend {
    /// every blob lives under here; nothing else is touched
    root: PathBuf,
    /// one line per operation, in call order
    ops: Mutex<Vec<String>>,
}

impl BlobStoreBackend {
    fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        // confine to the canonical root so a crafted name can never
        // steer a write outside of it
        Ok(Self {
            root: std::fs::canonicalize(&root)?,
            ops: Mutex::new(Vec::new()),
        })
    }

    fn record(&self, op: String) {
        if let Ok(mut ops) = self.ops.lock() {
            ops.push(op);
        }
    }

    fn op_log(&self) -> Vec<String> {
        self.ops.lock().map(|ops| ops.clone()).unwrap_or_default()
    }

    /// Where the blob for `namespace`/`name` lives. The manager validates
    /// both before they reach a backend, and we refuse the dangerous
    /// shapes again rather than trust that caller.
    fn blob_path(&self, namespace: &str, name: &str) -> Result<PathBuf, BackendError> {
        for (what, part) in [("namespace", namespace), ("name", name)] {
            let bad =
                part.is_empty() || part == "." || part == ".." || part.contains(['/', '\\', ':']);
            if bad {
                return Err(BackendError::Rejected {
                    backend: self.name().into(),
                    reason: format!("unsafe {what} for a blob path: `{part}`"),
                });
            }
        }
        Ok(self.root.join(namespace).join(format!("{name}.blob")))
    }
}

impl ResourceBackend for BlobStoreBackend {
    fn name(&self) -> &str {
        "blob-store"
    }

    fn create(&self, request: &ResourceRequest) -> cachegirl::Result<ResourceMeta> {
        if !matches!(request.kind, ResourceKind::Custom) {
            return Err(ResourceCreationError::UnsupportedKind {
                backend: self.name().into(),
                kind: request.kind.to_string(),
            }
            .into());
        }
        let path = self.blob_path(&request.namespace, &request.name)?;
        let parent = path.parent().expect("blob path always has a parent");
        std::fs::create_dir_all(parent).map_err(|source| BackendError::Io {
            backend: self.name().into(),
            source,
        })?;
        // never clobber whatever is already there
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| match source.kind() {
                ErrorKind::AlreadyExists => BackendError::Rejected {
                    backend: self.name().into(),
                    reason: format!("`{}` already exists", path.display()),
                },
                _ => BackendError::Io {
                    backend: self.name().into(),
                    source,
                },
            })?;

        self.record(format!(
            "create {}/{} -> {}",
            request.namespace,
            request.name,
            path.display()
        ));

        // empty at creation; the blob grows once the application writes
        // through the path recorded in the registry
        let mut meta = ResourceMeta::new(&request.name, &request.namespace, request.kind);
        meta.metadata
            .insert("path".into(), path.to_string_lossy().into_owned());
        for (k, v) in &request.metadata {
            meta.metadata.insert(k.clone(), v.clone());
        }
        Ok(meta)
    }

    fn release(&self, meta: &ResourceMeta) -> cachegirl::Result<()> {
        let path = match meta.metadata.get("path") {
            Some(p) => PathBuf::from(p),
            None => {
                return Err(BackendError::Rejected {
                    backend: self.name().into(),
                    reason: format!("resource `{}` carries no blob path", meta.id),
                }
                .into());
            }
        };
        // never act outside the confined root, even if the record was mangled
        if !path_is_within(&path, &self.root) {
            return Err(BackendError::Rejected {
                backend: self.name().into(),
                reason: format!(
                    "refusing to release `{}`: outside the blob store",
                    path.display()
                ),
            }
            .into());
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => self.record(format!(
                "release {}/{} ({size} bytes)",
                meta.namespace, meta.name
            )),
            // release must be idempotent; cleanup passes may see the same record twice
            Err(e) if e.kind() == ErrorKind::NotFound => self.record(format!(
                "release {}/{} (already gone)",
                meta.namespace, meta.name
            )),
            Err(source) => {
                return Err(BackendError::Io {
                    backend: self.name().into(),
                    source,
                }
                .into());
            }
        }
        Ok(())
    }
}

/// Lexical confinement check: `path` must sit under `root` after
/// resolving `.` and `..` components without touching the filesystem.
fn path_is_within(path: &Path, root: &Path) -> bool {
    let mut settled = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => settled.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !settled.pop() {
                    return false;
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                settled.push(component.as_os_str())
            }
        }
    }
    settled.starts_with(root)
}

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-custom-backend-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    // an in-memory event sink so we can see what the manager emitted
    let sink = Arc::new(VecSink::new());
    let manager = TempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .instance_id("instance-custom-backend")
        .event_sink(Arc::clone(&sink) as Arc<dyn cachegirl::EventSink>)
        .build()?;

    // 1 + 2: build the backend and hand it to the manager
    let blob_root = root.join("blob-store");
    let backend = Arc::new(BlobStoreBackend::new(&blob_root).expect("blob store is creatable"));
    manager.register_backend(Arc::clone(&backend) as Arc<dyn ResourceBackend>)?;
    println!(
        "registered backends: {:?}",
        vec!["filesystem", "blob-store"]
    );

    let jobs = manager.namespace("jobs")?;

    // 3: create a resource through it
    let blob = jobs.create_custom_with(
        "nightly-sync",
        "blob-store",
        CreateOptions::new()
            .owner("batch-runner")
            .metadata("job", "nightly-sync")
            .metadata("attempt", "1"),
    )?;
    let blob_id = blob.id().clone();
    println!(
        "created custom resource {} (backend: {})",
        blob_id,
        blob.backend()
    );

    // the blob path the backend recorded is visible through the registry;
    // write real content into it
    let meta = manager.registry().get(&blob_id)?;
    let path = PathBuf::from(
        meta.metadata
            .get("path")
            .expect("blob-store records a path in the registry record"),
    );
    std::fs::write(&path, vec![0xABu8; 2048]).expect("blob is writable");
    println!("wrote 2048 bytes to {}", path.display());

    // 4: release flows back into the blob store
    // note: bytes_freed in the manager's bookkeeping is whatever the
    // backend reported at creation (0 here - the blob was empty); the
    // backend's own op log shows the real on-disk size it reclaimed
    let released = jobs.release(&blob_id)?;
    println!(
        "release -> {released} (blob gone from disk: {})",
        !path.exists()
    );

    // a backend name nobody registered is a clean, typed error - not a panic
    match jobs.create_custom("orphan", "no-such-backend") {
        Ok(_) => println!("created against an unknown backend?!"),
        Err(e) => println!("unknown backend refused as expected: {e}"),
    }

    println!("\nbackend op log:");
    for line in backend.op_log() {
        println!("  {line}");
    }

    println!("\nmanager events:");
    for event in sink.drain() {
        match event {
            Event::ResourceCreated {
                kind, namespace, ..
            } => {
                println!("  ResourceCreated(kind={kind}, namespace={namespace})")
            }
            Event::ResourceReleased {
                id, bytes_freed, ..
            } => {
                println!("  ResourceReleased(id={id}, bytes_freed={bytes_freed})")
            }
            other => println!("  {other:?}"),
        }
    }

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
