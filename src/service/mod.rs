//! Transaction processing service with batched execution and snapshot output.

pub mod config;
pub mod error;
pub mod pending_transaction;

use crate::domain::{Account, ClientId, TransactionType};
use crate::message::{InputMessage, OutputMessage};
use crate::metrics::{outcome_kv, tx_type_kv, METRICS};
pub(crate) use crate::service::config::ServiceConfig;
use crate::service::error::TransactionError;
use crate::service::pending_transaction::PendingTransaction;
use crate::{CsvWriter, TransactionId};
use anyhow::{anyhow, Context, Result as AnyResult};
use dashmap::DashMap;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncWrite;
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot::Sender;
use tokio_util::sync::CancellationToken;
use tracing::trace;

/// The core transaction processing engine.
///
/// Receives messages via a channel, buffers them in a `BinaryHeap` for
/// chronological ordering, and flushes batches of transactions to per-client accounts.
#[derive(Default)]
pub struct Service {
    /// Batch size and other tuning parameters.
    config: ServiceConfig,
    /// Concurrent map of client accounts.
    accounts: DashMap<ClientId, Account>,
}

impl Service {
    /// Creates a new service wrapped in an `Arc` for shared ownership across tasks.
    pub fn new(config: ServiceConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            ..Default::default()
        })
    }

    /// Runs the event loop, receiving messages until cancellation or channel close.
    /// Flushes any remaining buffered messages before returning.
    pub async fn server_forever(
        self: Arc<Self>,
        ctx: CancellationToken,
        mut rx: UnboundedReceiver<ServiceMessage>,
    ) -> AnyResult<()> {
        let mut ordered_messages: BinaryHeap<PendingTransaction> = BinaryHeap::new();
        loop {
            select! {
                _ = ctx.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                       None => break,
                       Some(msg) => {
                           METRICS.service_ticks.add(1, &[]);
                           self.iteration(msg, &mut ordered_messages).await?;
                       }
                    }
                }
            }
        }
        self.handle_buffer(&mut ordered_messages).await?;
        Ok(())
    }

    /// Handles a single incoming service message: buffers transactions or processes a batch completion signal.
    async fn iteration(
        &self,
        msg: ServiceMessage,
        buffer: &mut BinaryHeap<PendingTransaction>,
    ) -> AnyResult<()> {
        match msg {
            ServiceMessage::Incoming(m, c) => {
                buffer.push(PendingTransaction {
                    message: m,
                    callback: c,
                });
                if buffer.len() >= self.config.batch_size {
                    self.handle_buffer(buffer).await?;
                }
                Ok(())
            }
            ServiceMessage::TransactionBatchCompletion(callback) => {
                // Process the buffer before confirming
                self.handle_buffer(buffer).await?;
                callback
                    .send(())
                    .map_err(|_| anyhow!("Failed to send callback due to closed receiver"))?;
                Ok(())
            }
        }
    }

    /// Processes the messages in the buffer.
    ///
    /// Using a buffer can reduce contention on accounts between writes and reads,
    ///
    /// In this particular take home test we don't have contention, but the mechanism is in place.
    async fn handle_buffer(&self, buffer: &mut BinaryHeap<PendingTransaction>) -> AnyResult<()> {
        let batch_size = buffer.len();
        if batch_size == 0 {
            return Ok(());
        }

        let start = Instant::now();
        METRICS.service_batches_processed.add(1, &[]);
        METRICS.service_batch_size.record(batch_size as f64, &[]);

        let mut processed = 0u64;
        let mut failed = 0u64;

        while let Some(tx) = buffer.pop() {
            let msg = &tx.message;
            let client_id = msg.client_id;
            let transaction_id = msg.transaction_id;
            let tx_type = msg.transaction_type;
            let mut entry = self
                .accounts
                .entry(msg.client_id)
                .or_insert_with(|| Account::new(msg.client_id));

            let res = match tx_type {
                TransactionType::Deposit => entry.process_deposit(msg),
                TransactionType::Withdrawal => entry.process_withdrawal(msg),
                TransactionType::Dispute => entry.process_dispute(msg),
                TransactionType::Resolve => entry.process_resolve(msg),
                TransactionType::Chargeback => entry.process_chargeback(msg),
            };

            let type_label = match tx_type {
                TransactionType::Deposit => "deposit",
                TransactionType::Withdrawal => "withdrawal",
                TransactionType::Dispute => "dispute",
                TransactionType::Resolve => "resolve",
                TransactionType::Chargeback => "chargeback",
            };

            match &res {
                Ok(()) => {
                    METRICS
                        .service_transactions_processed
                        .add(1, &[tx_type_kv(type_label), outcome_kv("ok")]);
                    processed += 1;
                }
                Err(e) => {
                    METRICS.service_transactions_processed.add(
                        1,
                        &[
                            tx_type_kv(type_label),
                            outcome_kv("error"),
                            opentelemetry::KeyValue::new("error", e.to_string()),
                        ],
                    );
                    failed += 1;
                }
            }

            tx.callback
                .send((client_id, transaction_id, res))
                .context(anyhow!(
                    "Failed to send transaction resolution callback due to closed receiver"
                ))?;
        }

        let elapsed = start.elapsed();
        METRICS
            .service_batch_duration_ms
            .record(elapsed.as_secs_f64() * 1000.0, &[]);
        METRICS.service_batch_transactions_ok.add(processed, &[]);
        METRICS.service_batch_transactions_failed.add(failed, &[]);
        METRICS
            .service_accounts_total
            .record(self.accounts.len() as f64, &[]);
        Ok(())
    }

    /// Serializes all account states to CSV via the given async writer.
    pub async fn write_snapshot<Writer>(&self, writer: Writer) -> AnyResult<()>
    where
        Writer: AsyncWrite + Unpin,
    {
        let start = Instant::now();
        let mut csv_writer = CsvWriter::new(writer);
        csv_writer.write_header().await?;
        let count = self.accounts.len();
        trace!(count, "Serializing accounts");
        let mut locked_count = 0u64;
        for entry in self.accounts.iter() {
            if entry.value().locked {
                locked_count += 1;
            }
            csv_writer
                .write_message(OutputMessage::from(entry.value()))
                .await
                .context("Unable to serialize output message")?;
        }
        csv_writer.flush().await?;
        let elapsed = start.elapsed();
        METRICS.snapshot_accounts_written.record(count as f64, &[]);
        METRICS
            .snapshot_accounts_locked
            .record(locked_count as f64, &[]);
        METRICS
            .snapshot_duration_ms
            .record(elapsed.as_secs_f64() * 1000.0, &[]);
        trace!("Flushed");
        Ok(())
    }
}

/// Messages sent to the [`Service`] event loop.
pub enum ServiceMessage {
    /// A transaction to be buffered and processed.
    Incoming(
        Box<InputMessage>,
        UnboundedSender<(ClientId, TransactionId, Result<(), TransactionError>)>,
    ),
    /// Signal that all transactions have been loaded; flushes the buffer and acknowledges via the oneshot.
    TransactionBatchCompletion(Sender<()>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::amount::Amount;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::sync::mpsc::UnboundedReceiver;

    /// Test helper that creates [`ServiceMessage`]s with auto-incrementing chrono order.
    #[derive(Default)]
    pub struct MessageSequencer(u64);

    impl MessageSequencer {
        /// Builds a [`ServiceMessage::Incoming`] with the next chrono order value.
        pub fn create_message(
            &mut self,
            client_id: ClientId,
            transaction_id: TransactionId,
            amount: i64,
            transaction_type: TransactionType,
            sx: UnboundedSender<(ClientId, TransactionId, Result<(), TransactionError>)>,
        ) -> ServiceMessage {
            let chrono_order = self.0;
            self.0 += 1;
            ServiceMessage::Incoming(
                Box::new(InputMessage {
                    chrono_order,
                    transaction_type,
                    client_id,
                    transaction_id,
                    amount: Amount::from_major(amount),
                }),
                sx,
            )
        }
    }

    /// Drains all pending messages from the channel, waiting up to 5s for each one.
    /// Drop the sender before calling so the function returns as soon as the channel
    /// is empty rather than blocking for the full timeout.
    async fn drain_channel(
        mut rx: UnboundedReceiver<(ClientId, TransactionId, Result<(), TransactionError>)>,
    ) -> Vec<(ClientId, TransactionId, Result<(), TransactionError>)> {
        let mut results = vec![];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Some(msg)) => results.push(msg),
                _ => break,
            }
        }
        results
    }

    #[tokio::test]
    async fn test_ordering_in_buffer() -> AnyResult<()> {
        let service = Service::new(ServiceConfig {
            // We set a batch size so the buffer does not get processed while we investigate ordering
            batch_size: 10,
            ..Default::default()
        });
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        let service_message = move |index: u32| -> ServiceMessage {
            ServiceMessage::Incoming(
                Box::new(InputMessage {
                    chrono_order: index as u64,
                    transaction_type: TransactionType::Deposit,
                    client_id: 1,
                    transaction_id: index,
                    amount: Amount::from_major(10u32.pow(index) as i64),
                }),
                sx.clone(),
            )
        };
        service.iteration(service_message(2), &mut buffer).await?;
        service.iteration(service_message(0), &mut buffer).await?;
        service.iteration(service_message(1), &mut buffer).await?;
        let order: Vec<_> = buffer
            .into_sorted_vec()
            .into_iter()
            .map(|i| i.message.chrono_order)
            .collect();
        // Reversed Ord for min-heap behavior: into_sorted_vec gives descending chrono_order
        assert_eq!(order, vec![2, 1, 0]);
        // No handle_buffer call, so no callbacks were sent
        drop(service_message);
        let results = drain_channel(rx).await;
        assert_eq!(results, vec![]);
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_buffer_empty() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut buffer = BinaryHeap::new();
        let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
        service
            .iteration(ServiceMessage::TransactionBatchCompletion(tx), &mut buffer)
            .await?;
        assert!(service.accounts.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_buffer_with_deposit() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut buffer = BinaryHeap::new();
        let mut seq = MessageSequencer::default();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(100));
        assert_eq!(account.total, Amount::from_major(100));
        assert_eq!(account.held, Amount::from_major(0));
        assert!(!account.locked);
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(results, vec![(1, 1, Ok(()))]);
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_buffer_with_withdrawal() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut buffer = BinaryHeap::new();
        let mut seq = MessageSequencer::default();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 2, 30, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(100));
        assert_eq!(account.total, Amount::from_major(100));
        drop(sx);
        let results = drain_channel(rx).await;
        // Oldest-first: withdrawal(chrono=0) before deposit(chrono=1)
        assert_eq!(
            results,
            vec![
                (1, 2, Err(TransactionError::InsufficientBalance)),
                (1, 1, Ok(())),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_buffer_multiple_clients() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(2, 2, 200, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account1 = service.accounts.get(&1).unwrap();
        let account2 = service.accounts.get(&2).unwrap();
        assert_eq!(account1.available, Amount::from_major(100));
        assert_eq!(account2.available, Amount::from_major(200));
        drop(sx);
        let results = drain_channel(rx).await;
        // Oldest-first: client=1 tx=1 (chrono=0) before client=2 tx=2 (chrono=1)
        assert_eq!(results, vec![(1, 1, Ok(())), (2, 2, Ok(())),]);
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_partial() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 200, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(300));
        assert_eq!(account.held, Amount::from_major(500));
        assert_eq!(account.total, Amount::from_major(800));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 2, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_full_available() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(500));
        assert_eq!(account.held, Amount::from_major(500));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(results, vec![(1, 1, Ok(())), (1, 1, Ok(())),]);
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_exceeds_available() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 800, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(0));
        assert_eq!(account.held, Amount::from_major(200));
        assert_eq!(account.total, Amount::from_major(200));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 2, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_non_existent_tx() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        // Create an account
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        // Create a dispute for non existing transaction
        service
            .iteration(
                seq.create_message(1, 999, 100, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        assert_eq!(service.accounts.len(), 1);
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        drop(sx);
        let results = drain_channel(rx).await;
        // Oldest-first: deposit(chrono=0) before dispute(chrono=1)
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 999, Err(TransactionError::InvalidTransaction)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_partial() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 300, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(800));
        assert_eq!(account.held, Amount::from_major(200));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_exceeds_disputed() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resolve_non_disputed() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        drop(sx);
        let results = drain_channel(rx).await;
        // Oldest-first: deposit(chrono=0) before resolve(chrono=1)
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Err(TransactionError::TransactionNotDisputed)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_chargeback_full_dispute() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(500));
        assert_eq!(account.held, Amount::from_major(0));
        assert_eq!(account.total, Amount::from_major(500));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_chargeback_partial_dispute() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 300, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(700));
        assert_eq!(account.held, Amount::from_major(0));
        assert_eq!(account.total, Amount::from_major(700));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_chargeback_non_disputed() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        assert_eq!(account.total, Amount::from_major(1000));
        drop(sx);
        let results = drain_channel(rx).await;
        // Oldest-first: deposit(chrono=0) before chargeback(chrono=1)
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Err(TransactionError::TransactionNotDisputed)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_chargeback_after_resolve() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 200, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        assert_eq!(account.total, Amount::from_major(1000));
        drop(sx);
        let results = drain_channel(rx).await;
        // deposit → dispute → resolve → chargeback (fails: Resolved can't transition to Chargeback)
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Err(TransactionError::TransactionNotDisputed)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_disputes_same_tx() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 300, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 200, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(500));
        assert_eq!(account.held, Amount::from_major(500));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 1, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_multiple_txs() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 500, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 300, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 400, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(300));
        assert_eq!(account.held, Amount::from_major(700));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 2, Ok(())),
                (1, 1, Ok(())),
                (1, 2, Ok(())),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_withdrawal_after() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 200, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(300));
        assert_eq!(account.held, Amount::from_major(500));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![(1, 1, Ok(())), (1, 1, Ok(())), (1, 2, Ok(())),]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_dispute_zero_amount() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 0, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(1000));
        assert_eq!(account.held, Amount::from_major(0));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(results, vec![(1, 1, Ok(())), (1, 1, Ok(())),]);
        Ok(())
    }

    #[tokio::test]
    async fn test_locked_account_rejects_all_operations() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        // Deposit, dispute, chargeback to lock the account
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        // Now try all operations on the locked account
        service
            .iteration(
                seq.create_message(1, 2, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 3, 50, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(500));
        assert_eq!(account.held, Amount::from_major(0));
        assert_eq!(account.total, Amount::from_major(500));
        assert!(account.locked);
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 2, Err(TransactionError::AccountLocked)),
                (1, 3, Err(TransactionError::AccountLocked)),
                (1, 1, Err(TransactionError::AccountLocked)),
                (1, 1, Err(TransactionError::AccountLocked)),
                (1, 1, Err(TransactionError::AccountLocked)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_duplicate_transaction_id() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        // Same tx_id=1, different amount — should be rejected as duplicate
        service
            .iteration(
                seq.create_message(1, 1, 200, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(100));
        assert_eq!(account.total, Amount::from_major(100));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Err(TransactionError::DuplicateTransaction)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_duplicate_withdrawal_id() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 100, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 2, 50, TransactionType::Withdrawal, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(900));
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 2, Ok(())),
                (1, 2, Err(TransactionError::DuplicateTransaction)),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_multi_client_dispute_isolation() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        // Two clients each deposit
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(2, 2, 2000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        // Dispute and chargeback on client 1 only
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        // Client 1 should be locked with reduced balance
        let account1 = service.accounts.get(&1).unwrap();
        assert_eq!(account1.available, Amount::from_major(500));
        assert_eq!(account1.held, Amount::from_major(0));
        assert_eq!(account1.total, Amount::from_major(500));
        assert!(account1.locked);
        // Client 2 should be completely unaffected
        let account2 = service.accounts.get(&2).unwrap();
        assert_eq!(account2.available, Amount::from_major(2000));
        assert_eq!(account2.held, Amount::from_major(0));
        assert_eq!(account2.total, Amount::from_major(2000));
        assert!(!account2.locked);
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (2, 2, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Ok(())),
            ]
        );
        Ok(())
    }

    // Assumption 5: a Resolved transaction can be re-disputed; the new dispute
    // accumulates on top (dispute → resolve → dispute sequence).
    #[tokio::test]
    async fn test_redispute_after_resolve() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 500, TransactionType::Resolve, sx.clone()),
                &mut buffer,
            )
            .await?;
        // Re-dispute the same transaction after it was resolved
        service
            .iteration(
                seq.create_message(1, 1, 300, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        let account = service.accounts.get(&1).unwrap();
        assert_eq!(account.available, Amount::from_major(700));
        assert_eq!(account.held, Amount::from_major(300));
        assert_eq!(account.total, Amount::from_major(1000));
        assert!(!account.locked);
        drop(sx);
        let results = drain_channel(rx).await;
        assert_eq!(
            results,
            vec![
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Ok(())),
                (1, 1, Ok(())),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_write_snapshot() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, _rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 100, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        drop(sx);
        let mut output = Vec::new();
        service.write_snapshot(&mut output).await?;
        let csv = String::from_utf8(output)?;
        let lines: Vec<&str> = csv.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "client,available,held,total,locked");
        assert_eq!(lines[1], "1,100.0000,0.0000,100.0000,false");
        Ok(())
    }

    #[tokio::test]
    async fn test_write_snapshot_locked_account() -> AnyResult<()> {
        let service = Service::new(ServiceConfig::default());
        let mut seq = MessageSequencer::default();
        let mut buffer = BinaryHeap::new();
        let (sx, _rx) = unbounded_channel();
        service
            .iteration(
                seq.create_message(1, 1, 1000, TransactionType::Deposit, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 400, TransactionType::Dispute, sx.clone()),
                &mut buffer,
            )
            .await?;
        service
            .iteration(
                seq.create_message(1, 1, 400, TransactionType::Chargeback, sx.clone()),
                &mut buffer,
            )
            .await?;
        service.handle_buffer(&mut buffer).await?;
        drop(sx);
        let mut output = Vec::new();
        service.write_snapshot(&mut output).await?;
        let csv = String::from_utf8(output)?;
        let lines: Vec<&str> = csv.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "client,available,held,total,locked");
        assert_eq!(lines[1], "1,600.0000,0.0000,600.0000,true");
        Ok(())
    }
}
