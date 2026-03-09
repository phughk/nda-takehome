use crate::domain::amount::Amount;
use crate::domain::{ClientId, TransactionId, TransactionType};
use crate::message::InputMessage;
use crate::metrics::METRICS;
use anyhow::{Context, Result};
use csv_async::AsyncReaderBuilder;
use futures::StreamExt;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct RawInputMessage {
    r#type: String,
    client: String,
    tx: String,
    amount: String,
}

#[derive(Debug)]
pub struct CsvReader {
    path: String,
}

impl CsvReader {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().display().to_string(),
        }
    }

    pub async fn read_messages(&self) -> Result<Vec<InputMessage>> {
        METRICS.csv_files_loaded.add(1, &[]);

        let file = tokio::fs::File::open(&self.path)
            .await
            .context(format!("Failed to open file: {}", self.path))?;

        let mut reader = AsyncReaderBuilder::new()
            .has_headers(true)
            .trim(csv_async::Trim::All)
            .create_deserializer(file);

        let mut messages = Vec::new();
        let mut chrono_order = 0u64;
        let mut records = reader.deserialize::<RawInputMessage>();
        while let Some(record_result) = records.next().await {
            let record: RawInputMessage = match record_result.context("Failed to deserialize record") {
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

            let message = InputMessage {
                chrono_order,
                transaction_type,
                client_id,
                transaction_id,
                amount,
            };

            messages.push(message);
            chrono_order += 1;
        }

        METRICS.csv_rows_parsed.add(chrono_order, &[]);

        Ok(messages)
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
        CsvReader::new(f.path()).read_messages().await
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
