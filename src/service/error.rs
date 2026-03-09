use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum TransactionError {
    #[error("Unable to withdraw due to insufficient balance")]
    InsufficientBalance,
    #[error("The account is locked")]
    AccountLocked,
    #[error("Transaction cannot be resolved because it is not disputed")]
    TransactionNotDisputed,
    #[error("Unable to resolve the transaction")]
    InvalidTransaction,
    #[error("Cannot add the transaction because it has already been added")]
    DuplicateTransaction,
    #[error("Unable to revert to a normal state")]
    CannotRevertToNormal,
}
