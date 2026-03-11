use crate::domain::amount::Amount;
use crate::domain::{ClientId, TransactionId, TransactionType};
use crate::Account;
use serde::{Serialize, Serializer};
use std::cmp::Ordering;

/// A parsed transaction from the CSV input.
///
/// Chrono order is the sequence number (total order) of the event/message.
/// `Ord` is reversed so a `BinaryHeap` processes oldest messages first.
#[derive(Debug, Clone)]
pub struct InputMessage {
    /// Monotonically increasing sequence number assigned at parse time.
    pub chrono_order: u64,
    /// The operation this message represents.
    pub transaction_type: TransactionType,
    /// The client this transaction belongs to.
    pub client_id: ClientId,
    /// Unique transaction identifier.
    pub transaction_id: TransactionId,
    /// The monetary amount for the operation.
    pub amount: Amount,
}

// TODO change this to actually be in a wrapped value
impl Ord for InputMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed for min-heap behavior with BinaryHeap (oldest-first processing).
        match other.chrono_order.cmp(&self.chrono_order) {
            Ordering::Equal => match other.client_id.cmp(&self.client_id) {
                Ordering::Equal => other.transaction_id.cmp(&self.transaction_id),
                order => order,
            },
            order => order,
        }
    }
}

impl PartialEq for InputMessage {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for InputMessage {}

impl PartialOrd for InputMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A snapshot of an account's balances for CSV output.
#[derive(Debug)]
pub struct OutputMessage<'a> {
    /// The client this snapshot belongs to.
    pub client_id: &'a ClientId,
    /// Funds available for withdrawal.
    pub available: &'a Amount,
    /// Funds held due to pending disputes.
    pub held: &'a Amount,
    /// Total funds (`available + held`).
    pub total: &'a Amount,
    /// Whether the account is frozen.
    pub locked: &'a bool,
}

impl<'a> Serialize for OutputMessage<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("OutputMessage", 5)?;
        state.serialize_field("client_id", &self.client_id)?;
        state.serialize_field("available", &self.available.to_string())?;
        state.serialize_field("held", &self.held.to_string())?;
        state.serialize_field("total", &self.total.to_string())?;
        state.serialize_field("locked", &self.locked)?;
        state.end()
    }
}

impl<'a> From<&'a Account> for OutputMessage<'a> {
    fn from(account: &'a Account) -> Self {
        Self {
            client_id: &account.client_id,
            available: &account.available,
            held: &account.held,
            total: &account.total,
            locked: &account.locked,
        }
    }
}
