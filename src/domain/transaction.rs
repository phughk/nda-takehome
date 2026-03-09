use serde::Deserialize;

pub type ClientId = u16;
pub type TransactionId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum TransactionType {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

impl TransactionType {
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
