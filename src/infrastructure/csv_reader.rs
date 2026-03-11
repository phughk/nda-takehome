use crate::domain::amount::Amount;
use crate::domain::{ClientId, TransactionId, TransactionType};
use crate::message::InputMessage;
use crate::metrics::METRICS;
use anyhow::{Context, Result};
use csv_async::AsyncReaderBuilder;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use std::path::Path;
use std::pin::Pin;

/// Raw CSV row before domain parsing.
#[derive(Debug, Deserialize)]
struct RawInputMessage {
    r#type: String,
    client: String,
    tx: String,
    amount: String,
}

/// Lazily reads a CSV file, yielding one [`InputMessage`] per call to [`next`](CsvReader::next).
pub struct CsvReader {
    /// Owned deserialized stream; boxed to erase the internal lifetime.
    records: Pin<Box<dyn Stream<Item = csv_async::Result<RawInputMessage>> + Send>>,
    /// Monotonically increasing position counter used for chrono ordering.
    chrono_order: u64,
    /// Set to `true` once the stream returns `None` so the metric is emitted only once.
    done: bool,
}

impl CsvReader {
    /// Opens the CSV file at `path` and prepares it for lazy row-by-row reading.
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        METRICS.csv_files_loaded.add(1, &[]);
        let path_display = path.as_ref().display().to_string();
        let file = tokio::fs::File::open(path.as_ref())
            .await
            .context(format!("Failed to open file: {}", path_display))?;

        let reader = AsyncReaderBuilder::new()
            .has_headers(true)
            .trim(csv_async::Trim::All)
            .create_deserializer(file);

        Ok(Self {
            records: Box::pin(reader.into_deserialize::<RawInputMessage>()),
            chrono_order: 0,
            done: false,
        })
    }

    /// Returns the next parsed [`InputMessage`], or `Ok(None)` when the file is exhausted.
    pub async fn next(&mut self) -> Result<Option<InputMessage>> {
        if self.done {
            return Ok(None);
        }

        match self.records.next().await {
            None => {
                METRICS.csv_rows_parsed.add(self.chrono_order, &[]);
                self.done = true;
                Ok(None)
            }
            Some(record_result) => {
                let record: RawInputMessage =
                    match record_result.context("Failed to deserialize record") {
                        Ok(r) => r,
                        Err(e) => {
                            METRICS.csv_parse_errors.add(1, &[]);
                            return Err(e);
                        }
                    };

                let transaction_type = TransactionType::from_str(&record.r#type)
                    .ok_or_else(|| anyhow::anyhow!("Invalid transaction type: {}", record.r#type))?;

                let client_id: ClientId = record.client.parse().context("Invalid client ID")?;

                let transaction_id: TransactionId =
                    record.tx.parse().context("Invalid transaction ID")?;

                let amount = if record.amount.is_empty() {
                    Amount::zero()
                } else {
                    Amount::parse(&record.amount)?
                };

                let chrono_order = self.chrono_order;
                self.chrono_order += 1;

                Ok(Some(InputMessage {
                    chrono_order,
                    transaction_type,
                    client_id,
                    transaction_id,
                    amount,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;
    use std::io::Write;
    use tempfile::NamedTempFile;

    async fn read_csv(content: &str) -> Result<Vec<InputMessage>> {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        let mut reader = CsvReader::new(f.path()).await?;
        let mut messages = Vec::new();
        while let Some(msg) = reader.next().await? {
            messages.push(msg);
        }
        Ok(messages)
    }

    // Assumption 22: empty amount field (dispute/resolve/chargeback rows) is
    // treated as zero rather than returning an error.
    #[tokio::test]
    async fn test_empty_amount_field_is_zero() {
        let csv = "type,client,tx,amount\ndispute,1,1,\nresolve,1,1,\nchargeback,1,1,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 3);
        for msg in &messages {
            assert_eq!(msg.amount, Amount::zero());
        }
    }

    #[tokio::test]
    async fn test_dispute_empty_amount_parses_transaction_fields() {
        let csv = "type,client,tx,amount\ndispute,42,99,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.transaction_type, TransactionType::Dispute);
        assert_eq!(msg.client_id, 42);
        assert_eq!(msg.transaction_id, 99);
        assert_eq!(msg.amount, Amount::zero());
    }

    #[tokio::test]
    async fn test_resolve_empty_amount_parses_transaction_fields() {
        let csv = "type,client,tx,amount\nresolve,7,3,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.transaction_type, TransactionType::Resolve);
        assert_eq!(msg.client_id, 7);
        assert_eq!(msg.transaction_id, 3);
        assert_eq!(msg.amount, Amount::zero());
    }

    #[tokio::test]
    async fn test_chargeback_empty_amount_parses_transaction_fields() {
        let csv = "type,client,tx,amount\nchargeback,5,10,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.transaction_type, TransactionType::Chargeback);
        assert_eq!(msg.client_id, 5);
        assert_eq!(msg.transaction_id, 10);
        assert_eq!(msg.amount, Amount::zero());
    }

    // Whitespace-only amount field should also be treated as zero.
    #[tokio::test]
    async fn test_whitespace_amount_field_is_zero() {
        let csv = "type,client,tx,amount\ndispute,1,1,   \nresolve,1,1,   \nchargeback,1,1,   \n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 3);
        for msg in &messages {
            assert_eq!(msg.amount, Amount::zero());
        }
    }

    // Mixed CSV: deposits/withdrawals have amounts, disputes/resolves/chargebacks do not.
    #[tokio::test]
    async fn test_mixed_rows_empty_and_non_empty_amounts() {
        let csv =
            "type,client,tx,amount\ndeposit,1,1,100.0\nwithdrawal,1,2,50.0\ndispute,1,1,\nresolve,1,1,\nchargeback,1,1,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].amount, Amount::from_major(100));
        assert_eq!(messages[1].amount, Amount::from_major(50));
        assert_eq!(messages[2].amount, Amount::zero());
        assert_eq!(messages[3].amount, Amount::zero());
        assert_eq!(messages[4].amount, Amount::zero());
    }

    // Chrono order is assigned sequentially regardless of amount presence.
    #[tokio::test]
    async fn test_chrono_order_assigned_for_empty_amount_rows() {
        let csv = "type,client,tx,amount\ndeposit,1,1,10\ndispute,1,1,\nresolve,1,1,\n";
        let messages = read_csv(csv).await.expect("should parse without error");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].chrono_order, 0);
        assert_eq!(messages[1].chrono_order, 1);
        assert_eq!(messages[2].chrono_order, 2);
    }

    // Assumption 24: an unrecognised transaction type is an error.
    #[tokio::test]
    async fn test_invalid_transaction_type_is_error() {
        let csv = "type,client,tx,amount\nrefund,1,1,100\n";
        let result = read_csv(csv).await;
        assert!(result.is_err(), "expected error for unknown transaction type");
    }

    #[test]
    fn test_four_decimal_places_input() {
        let cases = [
            ("5", Amount::from_major(5)),
            ("5.1", Amount::from_scaled(BigInt::from(51000))),
            ("5.1234", Amount::from_scaled(BigInt::from(51234))),
            ("5.12341", Amount::from_scaled(BigInt::from(51234))),
            ("5.12349", Amount::from_scaled(BigInt::from(51234))),
            ("0.00001", Amount::zero()),
            ("-0.00001", Amount::zero()),
        ];
        for (input, expected) in cases {
            let result = Amount::parse(input).unwrap();
            assert_eq!(result, expected, "Amount::parse({input:?})");
        }
    }
}
