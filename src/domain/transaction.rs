use serde::Deserialize;

/// Unique identifier for a client (max 65 535 clients).
pub type ClientId = u16;
/// Unique identifier for a transaction (max ~4 billion).
pub type TransactionId = u32;

/// The type of operation a transaction row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TransactionType {
    /// Credit funds into an account.
    Deposit,
    /// Debit funds from an account.
    Withdrawal,
    /// Open a dispute on an existing transaction.
    Dispute,
    /// Resolve an open dispute, returning held funds to available.
    Resolve,
    /// Charge back a disputed transaction, removing funds and locking the account.
    Chargeback,
}

impl TransactionType {
    /// Parses a lowercase string (e.g. `"deposit"`) into a [`TransactionType`].
    /// Returns `None` for unrecognised strings.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "deposit" => Some(TransactionType::Deposit),
            "withdrawal" => Some(TransactionType::Withdrawal),
            "dispute" => Some(TransactionType::Dispute),
            "resolve" => Some(TransactionType::Resolve),
            "chargeback" => Some(TransactionType::Chargeback),
            _ => None,
        }
    }
}
