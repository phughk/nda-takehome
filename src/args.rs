use clap::Parser;

#[derive(Parser)]
#[command(name = "nda-takehome")]
#[command(author, version, about, long_about = None)]
pub struct CliArgs {
    /// Input CSV file path containing transaction history
    #[arg(name = "path")]
    pub path: String,
}
