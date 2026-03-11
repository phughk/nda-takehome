//! OpenTelemetry metrics for observability across the pipeline.
//!
//! All instruments are initialised once via `lazy_static` and accessed through
//! the global [`METRICS`] singleton. An optional stdout exporter can be enabled
//! with the `metrics_stdout` feature flag.

use lazy_static::lazy_static;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::SdkMeterProvider;

lazy_static! {
    pub static ref EXPORTER: Option<SdkMeterProvider> = setup_exporter();
    pub static ref METRICS: Metrics = {
        let _initialise_exporter = &*EXPORTER;
        let meter = opentelemetry::global::meter("nda-takehome");
        Metrics {
            // CSV reader
            csv_files_loaded: meter.u64_counter("csv.files_loaded")
                .with_description("Number of CSV files loaded")
                .build(),
            csv_parse_errors: meter.u64_counter("csv.parse_errors")
                .with_description("Number of CSV rows that failed to parse")
                .build(),
            csv_rows_parsed: meter.u64_counter("csv.rows_parsed")
                .with_description("Total rows successfully parsed from CSV files")
                .build(),

            // Service event loop
            service_ticks: meter.u64_counter("service.ticks")
                .with_description("Total event loop iterations")
                .build(),
            service_accounts_total: meter.f64_gauge("service.accounts_total")
                .with_description("Total accounts at shutdown")
                .build(),

            // Batch processing
            service_batches_processed: meter.u64_counter("service.batches_processed")
                .with_description("Number of batch flushes")
                .build(),
            service_batch_size: meter.f64_histogram("service.batch_size")
                .with_description("Transactions per batch flush")
                .build(),
            service_batch_duration_ms: meter.f64_histogram("service.batch_duration_ms")
                .with_description("Time to process each batch")
                .with_unit("ms")
                .build(),
            service_transactions_processed: meter.u64_counter("service.transactions_processed")
                .with_description("Per-type transaction outcomes")
                .build(),
            service_batch_transactions_ok: meter.u64_counter("service.batch_transactions_ok")
                .with_description("Successful transactions across all batches")
                .build(),
            service_batch_transactions_failed: meter.u64_counter("service.batch_transactions_failed")
                .with_description("Failed transactions across all batches")
                .build(),

            // Snapshot writer
            snapshot_accounts_written: meter.f64_gauge("snapshot.accounts_written")
                .with_description("Accounts serialized to output")
                .build(),
            snapshot_accounts_locked: meter.f64_gauge("snapshot.accounts_locked")
                .with_description("Locked accounts at output time")
                .build(),
            snapshot_duration_ms: meter.f64_histogram("snapshot.duration_ms")
                .with_description("Time to write the snapshot")
                .with_unit("ms")
                .build(),

            // Pipeline (main)
            pipeline_duration: meter.f64_histogram("pipeline.total_duration_ms")
                .with_description("Wall-clock time for the full run")
                .with_unit("ms")
                .build(),
            pipeline_messages_enqueued: meter.u64_counter("pipeline.messages_enqueued")
                .with_description("Messages sent to the service")
                .build(),
            pipeline_load_duration: meter.f64_histogram("pipeline.load_duration_ms")
                .with_description("Time to read CSV and enqueue messages")
                .with_unit("ms")
                .build(),
            pipeline_transactions_ok: meter.u64_counter("pipeline.transactions_ok")
                .with_description("Successful outcomes observed by the outcome handler")
                .build(),
            pipeline_transactions_failed: meter.u64_counter("pipeline.transactions_failed")
                .with_description("Failed outcomes observed by the outcome handler")
                .build(),
        }
    };
}

/// Container for all OpenTelemetry metric instruments used across the application.
pub struct Metrics {
    // CSV reader
    /// Number of CSV files loaded.
    pub csv_files_loaded: Counter<u64>,
    pub csv_parse_errors: Counter<u64>,
    pub csv_rows_parsed: Counter<u64>,

    // Service event loop
    pub service_ticks: Counter<u64>,
    pub service_accounts_total: Gauge<f64>,

    // Batch processing
    pub service_batches_processed: Counter<u64>,
    pub service_batch_size: Histogram<f64>,
    pub service_batch_duration_ms: Histogram<f64>,
    pub service_transactions_processed: Counter<u64>,
    pub service_batch_transactions_ok: Counter<u64>,
    pub service_batch_transactions_failed: Counter<u64>,

    // Snapshot writer
    pub snapshot_accounts_written: Gauge<f64>,
    pub snapshot_accounts_locked: Gauge<f64>,
    pub snapshot_duration_ms: Histogram<f64>,

    // Pipeline (main)
    pub pipeline_duration: Histogram<f64>,
    pub pipeline_messages_enqueued: Counter<u64>,
    pub pipeline_load_duration: Histogram<f64>,
    pub pipeline_transactions_ok: Counter<u64>,
    pub pipeline_transactions_failed: Counter<u64>,
}

/// Helper to build a `KeyValue` pair for transaction type labels.
pub fn tx_type_kv(label: &'static str) -> KeyValue {
    KeyValue::new("type", label)
}

/// Helper to build a `KeyValue` pair for outcome labels.
pub fn outcome_kv(label: &'static str) -> KeyValue {
    KeyValue::new("outcome", label)
}

#[cfg(feature = "metrics_stdout")]
fn setup_exporter() -> Option<SdkMeterProvider> {
    use opentelemetry_sdk::{metrics::SdkMeterProvider, Resource};

    let exporter = opentelemetry_stdout::MetricExporterBuilder::default().build();
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_service_name("nda-takehome")
                .build(),
        )
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    Some(provider)
}

#[cfg(not(feature = "metrics_stdout"))]
fn setup_exporter() -> Option<SdkMeterProvider> {
    // No-op: metrics are recorded but not exported.
    None
}
