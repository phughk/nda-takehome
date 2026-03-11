use anyhow::{anyhow, Result as AnyResult};
use clap::Parser;
use futures::stream::FuturesUnordered;
use nda_takehome::cli::CliArgs;
use nda_takehome::infrastructure::CsvReader;
use nda_takehome::message::InputMessage;
use nda_takehome::metrics::{EXPORTER, METRICS};
use nda_takehome::service::config::ServiceConfig;
use nda_takehome::service::{Service, ServiceMessage};
use std::ops::Deref;
use std::path::PathBuf;
use std::time::Instant;
use tokio::select;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::level_filters::LevelFilter;
use tracing::{error, trace};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> AnyResult<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::OFF.into())
                .from_env_lossy(),
        )
        .init();
    let pipeline_start = Instant::now();
    let args = CliArgs::parse();
    let svc = Service::new(ServiceConfig::default());
    let ctx = CancellationToken::new();
    let mut tasks = FuturesUnordered::new();

    trace!("Starting service");
    let (svc_sx, svc_rx) = tokio::sync::mpsc::unbounded_channel();
    let svc_task = tokio::spawn(svc.clone().server_forever(ctx.clone(), svc_rx));
    tasks.push(svc_task);

    // We use this channel pair to track individual transaction outcomes
    let (tx_sx, mut tx_rx) = tokio::sync::mpsc::unbounded_channel();
    // Start a task to load the file asynchronously
    let (signal_sx, mut signal_rx) = tokio::sync::oneshot::channel();
    {
        trace!(args.input_file, "Loading file");
        let sx1 = svc_sx.clone();
        let sx2 = svc_sx.clone();
        let load_task = tokio::spawn(load_file(
            PathBuf::from(&args.input_file),
            async move |msg| {
                sx1.send(ServiceMessage::Incoming(msg, tx_sx.clone()))?;
                Ok(())
            },
            async move || {
                sx2.send(ServiceMessage::TransactionBatchCompletion(signal_sx))?;
                Ok(())
            },
        ));
        tasks.push(load_task);
    }
    // Start a task to log failed transactions/messages
    trace!("Starting transaction outcome handler");
    tasks.push(tokio::spawn(async move {
        let mut ok_count = 0u64;
        let mut err_count = 0u64;
        while let Some((client_id, tx_id, res)) = tx_rx.recv().await {
            match res {
                Ok(()) => ok_count += 1,
                Err(e) => {
                    err_count += 1;
                    let e_str = e.to_string();
                    error!(client_id, tx_id, e_str, "Transaction request failed");
                }
            }
        }
        // TODO add labels
        METRICS.pipeline_transactions_ok.add(ok_count, &[]);
        METRICS.pipeline_transactions_failed.add(err_count, &[]);
        Ok(())
    }));

    trace!("Waiting for completion");
    while !tasks.is_empty() {
        select! {
            r = tasks.next() => {
                // Handling of UnorderedFutures<TokioJoin<Task>> errors
                match r {
                    None => {
                        trace!("All tasks finished")
                        // TODO break ?? maybe
                    }
                    Some(Ok(Err(e))) => {
                        let e = e.context("Task returned error");
                        let error_msg = e.to_string();
                        error!(error_msg, "Task completed with result error");
                    }
                    Some(Err(e)) => {
                        let e = anyhow!(e).context("Tokio task failed");
                        let error_msg = e.to_string();
                        error!(error_msg, "Tokio task failed")
                    }
                    Some(Ok(Ok(_))) => {
                        trace!("Task completed successfully")
                    }
                }
            }
            completion_result = &mut signal_rx => {
                trace!("Received completion signal");
                completion_result?;
                svc.write_snapshot(tokio::io::stdout()).await?;
                ctx.cancel();
                break
            }
        }
    }

    let pipeline_elapsed = pipeline_start.elapsed();
    METRICS
        .pipeline_duration
        .record(pipeline_elapsed.as_secs_f64() * 1000.0, &[]);
    trace!(
        duration_ms = pipeline_elapsed.as_secs_f64() * 1000.0,
        "Completed"
    );
    if let Some(provider) = &*EXPORTER {
        // Force-flush metrics
        provider.shutdown()?;
    }
    Ok(())
}

/// Reads a CSV file lazily, sending each parsed message via `record_callback`,
/// then signals completion via `file_callback`.
async fn load_file<
    ItemCallback: AsyncFn(Box<InputMessage>) -> AnyResult<()>,
    FileCallback: AsyncFnOnce() -> AnyResult<()>,
>(
    path: PathBuf,
    record_callback: ItemCallback,
    file_callback: FileCallback,
) -> AnyResult<()> {
    let load_start = Instant::now();
    let mut reader = CsvReader::new(&path).await?;
    let mut msg_count = 0u64;

    while let Some(msg) = reader.next().await? {
        msg_count += 1;
        record_callback(Box::new(msg)).await?;
    }

    let load_elapsed = load_start.elapsed();
    METRICS.pipeline_messages_enqueued.add(msg_count, &[]);
    METRICS
        .pipeline_load_duration
        .record(load_elapsed.as_secs_f64() * 1000.0, &[]);

    file_callback().await?;
    Ok(())
}
