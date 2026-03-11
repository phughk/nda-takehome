//! A payments engine that processes CSV transaction streams.
//!
//! Reads deposits, withdrawals, disputes, resolves, and chargebacks from CSV
//! input, applies them to per-client accounts, and writes account snapshots
//! to CSV output.

pub mod cli;
pub mod domain;
pub mod infrastructure;
pub mod message;
pub mod metrics;
pub mod service;

pub use cli::CliArgs;
pub use domain::{Account, Amount, ClientId, TransactionId, TransactionType};
pub use infrastructure::{CsvReader, CsvWriter};
pub use message::{InputMessage, OutputMessage};
pub use service::config::ServiceConfig;
pub use service::{Service, ServiceMessage};
