use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::Temporality;
use serde_json::Value;
use std::time::Duration;

/// Exports metrics as `debug`-level tracing events — one log line per data
/// point. Output is gated by the tracing subscriber's level filter, so no
/// feature flag is required.
pub(super) struct TraceLevelExporter;

impl PushMetricExporter for TraceLevelExporter {
    fn export(&self, metrics: &ResourceMetrics) -> impl Future<Output = OTelSdkResult> + Send {
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                for entry in metric_as_log_entries(metric) {
                    tracing::debug!(name = metric.name(), entry, "metric");
                }
            }
        }
        async { Ok(()) }
    }

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Ok(())
    }

    fn temporality(&self) -> Temporality {
        Temporality::default()
    }
}

/// Serialises all data points in a metric to JSON strings, one per data point.
#[cfg(feature = "metrics_log")]
fn metric_as_log_entries(metric: &Metric) -> Vec<String> {
    match metric.data() {
        AggregatedMetrics::F64(t) => metric_as_log_entries_typed(metric.name(), t),
        AggregatedMetrics::U64(t) => metric_as_log_entries_typed(metric.name(), t),
        AggregatedMetrics::I64(t) => metric_as_log_entries_typed(metric.name(), t),
    }
}

#[cfg(feature = "metrics_log")]
fn metric_as_log_entries_typed<T: Copy + ToString>(
    name: &str,
    metric_data: &MetricData<T>,
) -> Vec<String> {
    match metric_data {
        MetricData::Gauge(g) => g
            .data_points()
            .map(|point| {
                let mut attributes = serde_json::value::Map::new();
                for kv in point.attributes() {
                    attributes.insert(kv.key.to_string(), Value::String(kv.value.to_string()));
                }
                let exemplars: Vec<String> =
                    point.exemplars().map(|d| d.value.to_string()).collect();
                serde_json::json!({
                    "name": name,
                    "value": point.value().to_string(),
                    "attributes": attributes,
                    "exemplars": exemplars,
                })
                .to_string()
            })
            .collect(),
        MetricData::Sum(s) => s
            .data_points()
            .map(|point| {
                let mut attributes = serde_json::value::Map::new();
                for kv in point.attributes() {
                    attributes.insert(kv.key.to_string(), Value::String(kv.value.to_string()));
                }
                let exemplars: Vec<String> =
                    point.exemplars().map(|d| d.value.to_string()).collect();
                serde_json::json!({
                    "name": name,
                    "value": point.value().to_string(),
                    "attributes": attributes,
                    "exemplars": exemplars,
                })
                .to_string()
            })
            .collect(),
        MetricData::Histogram(h) => h
            .data_points()
            .map(|point| {
                let mut attributes = serde_json::value::Map::new();
                for kv in point.attributes() {
                    attributes.insert(kv.key.to_string(), Value::String(kv.value.to_string()));
                }
                let exemplars: Vec<String> =
                    point.exemplars().map(|d| d.value.to_string()).collect();
                serde_json::json!({
                    "name": name,
                    "count": point.count(),
                    "sum": point.sum().to_string(),
                    "min": point.min().map(|v| v.to_string()),
                    "max": point.max().map(|v| v.to_string()),
                    "bounds": point.bounds().collect::<Vec<_>>(),
                    "bucket_counts": point.bucket_counts().collect::<Vec<_>>(),
                    "attributes": attributes,
                    "exemplars": exemplars,
                })
                .to_string()
            })
            .collect(),
        MetricData::ExponentialHistogram(h) => h
            .data_points()
            .map(|point| {
                let mut attributes = serde_json::value::Map::new();
                for kv in point.attributes() {
                    attributes.insert(kv.key.to_string(), Value::String(kv.value.to_string()));
                }
                let exemplars: Vec<String> =
                    point.exemplars().map(|d| d.value.to_string()).collect();
                serde_json::json!({
                    "name": name,
                    "count": point.count(),
                    "sum": point.sum().to_string(),
                    "min": point.min().map(|v| v.to_string()),
                    "max": point.max().map(|v| v.to_string()),
                    "scale": point.scale(),
                    "zero_count": point.zero_count(),
                    "zero_threshold": point.zero_threshold(),
                    "positive_offset": point.positive_bucket().offset(),
                    "positive_counts": point.positive_bucket().counts().collect::<Vec<_>>(),
                    "negative_offset": point.negative_bucket().offset(),
                    "negative_counts": point.negative_bucket().counts().collect::<Vec<_>>(),
                    "attributes": attributes,
                    "exemplars": exemplars,
                })
                .to_string()
            })
            .collect(),
    }
}
