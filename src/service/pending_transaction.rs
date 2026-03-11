use crate::service::error::TransactionError;
use crate::{ClientId, InputMessage, TransactionId};
use std::cmp::Ordering;
use tokio::sync::mpsc::UnboundedSender;

/// A buffered transaction awaiting processing, paired with a callback channel
/// for reporting the outcome. Ordered by the underlying [`InputMessage`] for heap-based sorting.
pub struct PendingTransaction {
    /// The transaction message to process.
    pub message: Box<InputMessage>,
    /// Channel to send the transaction result back to the caller.
    pub callback: UnboundedSender<(ClientId, TransactionId, Result<(), TransactionError>)>,
}

impl PartialOrd for PendingTransaction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingTransaction {
    fn cmp(&self, other: &Self) -> Ordering {
        self.message.cmp(&other.message)
    }
}

impl Eq for PendingTransaction {}

impl PartialEq for PendingTransaction {
    fn eq(&self, other: &Self) -> bool {
        self.message == other.message
    }
}
