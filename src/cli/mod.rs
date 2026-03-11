//! Command-line argument parsing.

use clap::Parser;

/// Command-line arguments for the payments engine.
#[derive(Parser, Debug)]
#[command(name = "nda-takehome")]
#[command(about = "A payments engine for processing transactions", long_about = None)]
pub struct CliArgs {
    /// Input CSV file path
    #[arg()]
    pub input_file: String,
}
