//! Two namespaces with two different sets of rules, and full isolation
//! between them.
//!
//! `network` gets a byte quota and free-form metadata; `protocol` gets a
//! count quota. Creating, measuring and sweeping on one side is invisible
//! on the other: a namespace can never touch the resources of another.
//!
//! ```sh
//! cargo run --example namespace
//! ```

use std::time::Duration;

use cachegirl::{NamespaceQuota, QuotaError, TempManager};

fn main() -> cachegirl::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "cachegirl-namespace-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let manager = TempManager::builder()
        .root(&root)
        .create_if_missing(true)
        .cleanup_on_shutdown(true)
        .build()?;

    let network = manager.namespace("network")?;
    let protocol = manager.namespace("protocol")?;

    // -- per-namespace quotas ---------------------------------------------
    // bytes for network, count for protocol; neither affects the other
    network.set_quota(NamespaceQuota::with_max_bytes(4096));
    protocol.set_quota(NamespaceQuota::with_max_resources(2));
    println!("network quota: {:?}", network.quota());
    println!("protocol quota: {:?}", protocol.quota());

    // -- metadata on the namespace itself ----------------------------------
    network.set_metadata("team", "net-eng");
    network.set_metadata("purpose", "ingress scratch space");
    protocol.set_metadata("team", "proto-eng");
    protocol.set_metadata("purpose", "wire-format scratch");
    println!("network meta team = {:?}", network.metadata("team"));
    println!("protocol meta purpose = {:?}", protocol.metadata("purpose"));

    // -- network side: file that outgrows the byte quota ------------------
    let payload = network.create_file("payload.bin")?;
    std::fs::write(payload.path(), vec![7u8; 8192]).expect("payload is writable");
    let payload_id = payload.id().clone();
    // tracked size starts at 0 for files; re-stat to sync it
    let tracked = network.refresh_size(&payload_id)?;
    let (bytes, count) = network.usage()?;
    println!(
        "network usage after write: {bytes} bytes across {count} resource(s) (tracked {tracked})"
    );

    // over quota: one live 8 KiB file against a 4 KiB cap, so the next
    // pass evicts it (least-recently-used first) to get back under the limit
    let report = network.cleanup_expired()?;
    println!(
        "network sweep: scanned {}, removed {} (size_eviction={}), bytes freed {}",
        report.scanned, report.removed, report.reasons.size_eviction, report.bytes_freed
    );
    assert_eq!(report.reasons.size_eviction, 1);

    // -- an anchor resource in network for the isolation check -------------
    let anchor = network.create_file("anchor.bin")?;

    // -- protocol side: count quota enforcement ----------------------------
    let p1 = protocol.create_file("frame-1.bin")?;
    let p2 = protocol.create_file("frame-2.bin")?;
    println!(
        "protocol created frame-1 ({}) and frame-2 ({})",
        p1.id(),
        p2.id()
    );
    match protocol.create_file("frame-3.bin") {
        Ok(_) => println!("protocol created frame-3 unexpectedly"),
        Err(cachegirl::Error::Quota(QuotaError::CountExceeded { limit })) => {
            println!("protocol rejected frame-3: count quota ({limit}) hit")
        }
        Err(e) => println!("protocol rejected frame-3 with unexpected error: {e}"),
    }

    // -- isolation ----------------------------------------------------------
    // protocol can see its own resources only; the network id means nothing to it
    println!(
        "protocol resources before release: {}",
        protocol.resources()?.len()
    );
    match protocol.release(anchor.id()) {
        Ok(_) => println!("protocol release of network's id: accepted?!"),
        Err(e) => println!("protocol release of network's id: refused ({e})"),
    }

    // free the count slots for the next step
    protocol.release(p1.id())?;
    protocol.release(p2.id())?;
    println!("protocol released frame-1 and frame-2");

    // -- namespace-scoped sweep ---------------------------------------------
    // expire one resource in each namespace, then sweep protocol only
    let net_ttl = network.create_file_with(
        "net-short.bin",
        cachegirl::CreateOptions::new().ttl(Duration::from_millis(150)),
    )?;
    let proto_ttl = protocol.create_file_with(
        "proto-short.bin",
        cachegirl::CreateOptions::new().ttl(Duration::from_millis(150)),
    )?;
    std::thread::sleep(Duration::from_millis(300));
    let report = protocol.cleanup_expired()?;
    println!(
        "protocol-scoped sweep: scanned {}, removed {} (ttl.fixed={})",
        report.scanned, report.removed, report.reasons.ttl_fixed
    );
    assert!(
        net_ttl.path().exists(),
        "protocol's sweep must not touch network"
    );
    assert!(
        !proto_ttl.path().exists(),
        "protocol's sweep must reap its own"
    );

    let (net_bytes, net_count) = network.usage()?;
    let (proto_bytes, proto_count) = protocol.usage()?;
    println!(
        "final usage: network ({net_bytes} bytes, {net_count} resources), protocol ({proto_bytes} bytes, {proto_count} resources)"
    );

    manager.shutdown()?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
