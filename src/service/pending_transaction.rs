use crate::service::error::TransactionError;
use crate::{ClientId, InputMessage, TransactionId};
use std::cmp::Ordering;
use tokio::sync::mpsc::UnboundedSender;

/// Used to track messages in the buffer
pub struct PendingTransaction {
    pub message: Box<InputMessage>,
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
