use crate::message::OutputMessage;
use anyhow::{Context, Result};
use tokio::io::AsyncWrite;

/// Asynchronously serializes [`OutputMessage`]s to CSV format.
pub struct CsvWriter<W: AsyncWrite + Unpin> {
    /// The underlying async CSV serializer.
    writer: csv_async::AsyncSerializer<W>,
}

impl<W: AsyncWrite + Unpin> CsvWriter<W> {
    /// Creates a new CSV writer wrapping the given async writer.
    pub fn new(writer: W) -> Self {
        let builder = csv_async::AsyncWriterBuilder::new();
        let csv_writer = builder.create_serializer(writer);
        Self { writer: csv_writer }
    }

    /// Writes the CSV header row (`client,available,held,total,locked`).
    pub async fn write_header(&mut self) -> Result<()> {
        self.writer
            .serialize(("client", "available", "held", "total", "locked"))
            .await
            .context("Failed to serialise csv header")
    }

    /// Serializes a single account snapshot as a CSV row.
    pub async fn write_message<'a>(&mut self, message: OutputMessage<'a>) -> Result<()> {
        self.writer
            .serialize((
                message.client_id,
                message.available.to_string(),
                message.held.to_string(),
                message.total.to_string(),
                message.locked,
            ))
            .await
            .context("Failed to write CSV record")?;

        Ok(())
    }

    /// Flushes all buffered data to the underlying writer.
    pub async fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .await
            .context("Failed to flush CSV writer")?;
        Ok(())
    }
}
