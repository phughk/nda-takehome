use crate::domain::amount::Amount;
use crate::domain::{ClientId, TransactionId, TransactionType};
use crate::Account;
use serde::{Serialize, Serializer};
use std::cmp::Ordering;

/// Chrono order is the sequence number (total order) of the event/message.
/// It is at the top of the struct for derive Ord convenience
#[derive(Debug, Clone)]
pub struct InputMessage {
    pub chrono_order: u64,
    pub transaction_type: TransactionType,
    pub client_id: ClientId,
    pub transaction_id: TransactionId,
    pub amount: Amount,
}

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

#[derive(Debug)]
pub struct OutputMessage<'a> {
    pub client_id: &'a ClientId,
    pub available: &'a Amount,
    pub held: &'a Amount,
    pub total: &'a Amount,
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
