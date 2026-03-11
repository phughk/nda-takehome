use thiserror::Error;

/// Errors that can occur when processing a transaction against an account.
#[derive(Error, Debug, PartialEq)]
pub enum TransactionError {
    /// Withdrawal amount exceeds available funds.
    #[error("Unable to withdraw due to insufficient balance")]
    InsufficientBalance,
    /// The account is frozen due to a prior chargeback.
    #[error("The account is locked")]
    AccountLocked,
    /// A resolve or chargeback was attempted on a transaction that is not currently disputed.
    #[error("Transaction is not disputed")]
    TransactionNotDisputed,
    /// The referenced transaction ID does not exist in the account.
    #[error("Unable to resolve the transaction")]
    InvalidTransaction,
    /// A deposit or withdrawal reused an existing transaction ID.
    #[error("Cannot add the transaction because it has already been added")]
    DuplicateTransaction,
    /// A transition attempted to move a transaction back to the Normal state.
    #[error("Unable to revert to a normal state")]
    CannotRevertToNormal,
    #[error("Cannot accept transaction because the amount is negative")]
    AmountNegative,
    #[error("Transaction is already in a terminal state")]
    TransactionTerminalState,
    #[error("Cannot dispute an already disputed transaction")]
    AlreadyDisputed,
}
