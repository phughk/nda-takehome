use crate::message::OutputMessage;
use anyhow::{Context, Result};
use tokio::io::AsyncWrite;

pub struct CsvWriter<W: AsyncWrite + Unpin> {
    writer: csv_async::AsyncSerializer<W>,
}

impl<W: AsyncWrite + Unpin> CsvWriter<W> {
    pub fn new(writer: W) -> Self {
        let builder = csv_async::AsyncWriterBuilder::new();
        let csv_writer = builder.create_serializer(writer);
        Self { writer: csv_writer }
    }

    pub async fn write_header(&mut self) -> Result<()> {
        self.writer
            .serialize(("client", "available", "held", "total", "locked"))
            .await
            .context("Failed to serialise csv header")
    }

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

    pub async fn flush(&mut self) -> Result<()> {
        self.writer
            .flush()
            .await
            .context("Failed to flush CSV writer")?;
        Ok(())
    }
}
