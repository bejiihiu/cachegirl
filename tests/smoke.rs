//! External-crate smoke test: uses only the published API surface.

use cachegirl::{ResourceKind, TempManager};

#[test]
fn manager_namespace_cleanup_shutdown() {
    let dir = std::env::temp_dir().join(format!(
        "cachegirl-smoke-{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    ));

    let manager = TempManager::builder()
        .root(&dir)
        .create_if_missing(true)
        .cleanup_on_shutdown(true)
        .instance_id("smoke-instance")
        .build()
        .unwrap();

    let ns = manager.namespace("integration").unwrap();
    assert_eq!(ns.name(), "integration");
    assert_eq!(
        manager.namespaces().unwrap(),
        vec!["integration".to_string()]
    );

    let report = manager.cleanup_expired().unwrap();
    assert_eq!(report.removed, 0);

    let stats = manager.stats().unwrap();
    assert_eq!(stats.instance_id, "smoke-instance");
    assert_eq!(stats.namespaces, 1);

    manager.shutdown().unwrap();
    assert!(manager.namespace("too-late").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn public_api_create_release_flow() {
    let dir = std::env::temp_dir().join(format!(
        "cachegirl-smoke3-{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    ));

    let sink = std::sync::Arc::new(cachegirl::VecSink::new());
    let manager = TempManager::builder()
        .root(&dir)
        .create_if_missing(true)
        .instance_id("smoke-create")
        .default_ttl(std::time::Duration::from_secs(3600))
        .event_sink(sink.clone())
        .build()
        .unwrap();

    let protocol = manager.namespace("protocol").unwrap();
    let file = protocol.create_file("packet-dump.bin").unwrap();
    assert!(file.exists());

    let directory = protocol.create_directory("work").unwrap();
    assert!(directory.exists());

    #[cfg(feature = "memory")]
    {
        let mut buf = protocol.create_buffer(64 * 1024).unwrap();
        buf.extend_from_slice(b"ping");
        assert_eq!(buf.len(), 4);
    }

    let stats = manager.stats().unwrap();
    assert!(stats.resources >= 2);

    let id = file.id().clone();
    assert!(protocol.release(&id).unwrap());
    assert!(!file.exists());

    let events = sink.drain();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, cachegirl::Event::ResourceCreated { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, cachegirl::Event::ResourceReleased { .. }))
    );

    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registry_and_leases_work_across_handles() {
    let dir = std::env::temp_dir().join(format!(
        "cachegirl-smoke2-{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    ));

    let manager = TempManager::builder()
        .root(&dir)
        .instance_id("smoke-leases")
        .build()
        .unwrap();
    let registry = manager.registry();

    let meta = cachegirl::ResourceMeta::new("packet.bin", "integration", ResourceKind::File);
    let id = meta.id.clone();
    registry.register(meta).unwrap();

    let book = cachegirl::LeaseBook::new();
    let lease = book.acquire(&id).unwrap();
    assert_eq!(book.lease_count(&id).unwrap(), 1);
    drop(lease);
    assert_eq!(book.lease_count(&id).unwrap(), 0);

    let removed = registry.remove(&id).unwrap();
    assert_eq!(removed.name, "packet.bin");

    manager.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
