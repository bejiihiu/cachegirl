//! Metrics emission plumbing.
//!
//! Everything here is a no-op unless the `metrics` feature is enabled, so
//! call sites stay free of `cfg` blocks. Emission goes through the
//! `metrics` facade: the crate never installs a recorder, the
//! application picks the exporter.

use crate::core::resource::ResourceKind;

/// What happened to an operation, for the `result` label.
pub(crate) const RESULT_OK: &str = "ok";
/// Operation failed; see the surrounding event/log for details.
pub(crate) const RESULT_ERROR: &str = "error";

#[cfg(feature = "metrics")]
fn kind_str(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::File => "file",
        ResourceKind::Directory => "directory",
        ResourceKind::Buffer => "buffer",
        ResourceKind::Cache => "cache",
        ResourceKind::Serialization => "serialization",
        ResourceKind::Runtime => "runtime",
        ResourceKind::Custom => "custom",
    }
}

#[cfg(feature = "metrics")]
pub(crate) fn resource_created(namespace: &str, kind: ResourceKind, result: &'static str) {
    metrics::counter!(
        description: "Resources created, partitioned by namespace, kind and outcome",
        "cachegirl_resources_created_total",
        "namespace" => namespace.to_owned(),
        "kind" => kind_str(kind),
        "result" => result,
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn resource_created(_namespace: &str, _kind: ResourceKind, _result: &'static str) {}

#[cfg(feature = "metrics")]
pub(crate) fn resource_released(namespace: &str, kind: ResourceKind, result: &'static str) {
    metrics::counter!(
        description: "Resources released, partitioned by namespace, kind and outcome",
        "cachegirl_resources_released_total",
        "namespace" => namespace.to_owned(),
        "kind" => kind_str(kind),
        "result" => result,
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn resource_released(_namespace: &str, _kind: ResourceKind, _result: &'static str) {}

#[cfg(feature = "metrics")]
pub(crate) fn bytes_freed(bytes: u64) {
    metrics::counter!(
        description: "Bytes reclaimed by resource destruction",
        unit: metrics::Unit::Bytes,
        "cachegirl_bytes_freed_total",
    )
    .increment(bytes);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn bytes_freed(_bytes: u64) {}

#[cfg(feature = "metrics")]
pub(crate) fn active_resources_change(delta: f64) {
    let gauge = metrics::gauge!(
        description: "Resources currently tracked by the registry, across all managers",
        "cachegirl_active_resources",
    );
    if delta >= 0.0 {
        gauge.increment(delta);
    } else {
        gauge.decrement(-delta);
    }
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn active_resources_change(_delta: f64) {}

#[cfg(feature = "metrics")]
pub(crate) fn active_leases_change(delta: f64) {
    let gauge = metrics::gauge!(
        description: "Leases currently held, across all managers",
        "cachegirl_active_leases",
    );
    if delta >= 0.0 {
        gauge.increment(delta);
    } else {
        gauge.decrement(-delta);
    }
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn active_leases_change(_delta: f64) {}

#[cfg(feature = "metrics")]
pub(crate) fn cleanup_run(scope: &'static str) {
    metrics::counter!(
        description: "Cleanup passes executed, partitioned by scope",
        "cachegirl_cleanup_runs_total",
        "scope" => scope,
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn cleanup_run(_scope: &'static str) {}

#[cfg(feature = "metrics")]
pub(crate) fn cleanup_duration(scope: &'static str, duration: core::time::Duration) {
    metrics::histogram!(
        description: "Wall-clock duration of cleanup passes, in seconds",
        unit: metrics::Unit::Seconds,
        "cachegirl_cleanup_duration_seconds",
        "scope" => scope,
    )
    .record(duration.as_secs_f64());
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn cleanup_duration(_scope: &'static str, _duration: core::time::Duration) {}

#[cfg(feature = "metrics")]
pub(crate) fn quota_rejection(namespace: &str, reason: &'static str) {
    metrics::counter!(
        description: "Resource creations refused by a quota, partitioned by namespace and limit kind",
        "cachegirl_quota_rejections_total",
        "namespace" => namespace.to_owned(),
        "reason" => reason,
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn quota_rejection(_namespace: &str, _reason: &'static str) {}

#[cfg(feature = "metrics")]
pub(crate) fn deferred_lease_operation(operation: &'static str) {
    metrics::counter!(
        description: "Destructions deferred because leases were held, partitioned by the operation that deferred",
        "cachegirl_deferred_lease_operations_total",
        "operation" => operation,
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
#[inline(always)]
pub(crate) fn deferred_lease_operation(_operation: &'static str) {}
