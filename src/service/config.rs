/// Configuration for the transaction processing [`Service`](super::Service).
pub struct ServiceConfig {
    /// Number of messages to buffer before flushing a batch.
    pub batch_size: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { batch_size: 10 }
    }
}
