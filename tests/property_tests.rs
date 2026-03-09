use anyhow::Result as AnyResult;
use nda_takehome::service::config::ServiceConfig;
use nda_takehome::service::{Service, ServiceMessage};
use nda_takehome::CsvReader;
use proptest::prelude::*;
use std::collections::HashMap;
use std::io::Write;
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn tx_type_strategy() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        3 => Just("deposit"),
        2 => Just("withdrawal"),
        2 => Just("dispute"),
        1 => Just("resolve"),
        1 => Just("chargeback"),
    ]
}

fn amount_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Whole numbers
        (0u64..100_000u64).prop_map(|n| n.to_string()),
        // With decimals up to 4 places
        (0u64..100_000u64, 0u32..10_000u32).prop_map(|(whole, frac)| {
            format!("{}.{:04}", whole, frac)
        }),
        // Small fractional amounts
        (0u32..9999u32).prop_map(|frac| format!("0.{:04}", frac)),
        // Empty (for dispute/resolve/chargeback)
        Just(String::new()),
    ]
}

fn client_id_strategy() -> impl Strategy<Value = u16> {
    1u16..=5
}

fn tx_id_strategy() -> impl Strategy<Value = u32> {
    1u32..=20
}

/// A single CSV row (pre-header).
#[derive(Debug, Clone)]
struct CsvRow {
    tx_type: String,
    client: u16,
    tx: u32,
    amount: String,
}

impl CsvRow {
    fn to_csv_line(&self) -> String {
        format!("{}, {}, {}, {}", self.tx_type, self.client, self.tx, self.amount)
    }
}

fn csv_row_strategy() -> impl Strategy<Value = CsvRow> {
    (tx_type_strategy(), client_id_strategy(), tx_id_strategy(), amount_strategy()).prop_map(
        |(tx_type, client, tx, amount)| CsvRow {
            tx_type: tx_type.to_string(),
            client,
            tx,
            amount,
        },
    )
}

fn csv_rows_strategy(max_rows: usize) -> impl Strategy<Value = Vec<CsvRow>> {
    prop::collection::vec(csv_row_strategy(), 1..max_rows)
}

// ---------------------------------------------------------------------------
// Helpers: run the engine via CSV round-trip
// ---------------------------------------------------------------------------

/// Parsed output row from the engine.
#[derive(Debug)]
struct OutputRow {
    client: u16,
    available: String,
    held: String,
    total: String,
    locked: bool,
}

/// Build a CSV string from rows, feed it through the engine, return parsed output.
async fn run_engine(rows: &[CsvRow]) -> AnyResult<Vec<OutputRow>> {
    let mut csv_content = String::from("type, client, tx, amount\n");
    for row in rows {
        csv_content.push_str(&row.to_csv_line());
        csv_content.push('\n');
    }

    let mut tmpfile = NamedTempFile::new()?;
    tmpfile.write_all(csv_content.as_bytes())?;

    let reader = CsvReader::new(tmpfile.path());
    let messages = reader.read_messages().await?;

    let service = Service::new(ServiceConfig::default());
    let ctx = tokio_util::sync::CancellationToken::new();
    let (svc_sx, svc_rx) = tokio::sync::mpsc::unbounded_channel();
    let (tx_sx, mut tx_rx) = tokio::sync::mpsc::unbounded_channel();

    let svc_clone = service.clone();
    let svc_handle =
        tokio::spawn(async move { svc_clone.server_forever(ctx.clone(), svc_rx).await });

    for msg in messages {
        svc_sx.send(ServiceMessage::Incoming(Box::new(msg), tx_sx.clone()))?;
    }

    let (signal_sx, signal_rx) = tokio::sync::oneshot::channel();
    svc_sx.send(ServiceMessage::TransactionBatchCompletion(signal_sx))?;
    signal_rx.await?;

    drop(tx_sx);
    // Drain outcomes
    while tx_rx.recv().await.is_some() {}

    let mut output = Vec::new();
    service.write_snapshot(&mut output).await?;
    let csv_out = String::from_utf8(output)?;

    drop(svc_sx);
    let _ = svc_handle.await;

    let mut rdr = csv::Reader::from_reader(csv_out.as_bytes());
    let mut results = Vec::new();
    for record in rdr.deserialize::<HashMap<String, String>>() {
        let row = record?;
        results.push(OutputRow {
            client: row["client"].parse()?,
            available: row["available"].clone(),
            held: row["held"].clone(),
            total: row["total"].clone(),
            locked: row["locked"].parse()?,
        });
    }

    Ok(results)
}

fn parse_amount(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// The engine must never crash on any valid CSV input sequence.
    #[test]
    fn engine_never_panics(rows in csv_rows_strategy(50)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_engine(&rows));
        // The engine should succeed (errors in individual transactions are logged,
        // not propagated). A top-level error here means a crash/panic.
        prop_assert!(result.is_ok(), "Engine returned error: {:?}", result.err());
    }

    /// For every output account: total == available + held.
    #[test]
    fn total_equals_available_plus_held(rows in csv_rows_strategy(50)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();
        for acc in &accounts {
            let available = parse_amount(&acc.available);
            let held = parse_amount(&acc.held);
            let total = parse_amount(&acc.total);
            let diff = (total - (available + held)).abs();
            prop_assert!(
                diff < 0.00015,
                "client {}: total ({}) != available ({}) + held ({}), diff={}",
                acc.client, acc.total, acc.available, acc.held, diff
            );
        }
    }

    /// Available balance must never be negative.
    #[test]
    fn available_never_negative(rows in csv_rows_strategy(50)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();
        for acc in &accounts {
            let available = parse_amount(&acc.available);
            prop_assert!(
                available >= -0.00005,
                "client {}: negative available = {}",
                acc.client, acc.available
            );
        }
    }

    /// Held balance must never be negative.
    #[test]
    fn held_never_negative(rows in csv_rows_strategy(50)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();
        for acc in &accounts {
            let held = parse_amount(&acc.held);
            prop_assert!(
                held >= -0.00005,
                "client {}: negative held = {}",
                acc.client, acc.held
            );
        }
    }

    /// Total balance must never be negative.
    #[test]
    fn total_never_negative(rows in csv_rows_strategy(50)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();
        for acc in &accounts {
            let total = parse_amount(&acc.total);
            prop_assert!(
                total >= -0.00005,
                "client {}: negative total = {}",
                acc.client, acc.total
            );
        }
    }

    /// Deposit-only sequences: final balance equals sum of all deposits per client.
    #[test]
    fn deposit_only_sum_is_correct(
        deposits in prop::collection::vec(
            (client_id_strategy(), 1u32..=1000u32, 1u64..=10_000u64),
            1..30
        )
    ) {
        // Build deposit-only rows with unique tx IDs
        let mut rows = Vec::new();
        let mut next_tx = 1u32;
        let mut expected: HashMap<u16, f64> = HashMap::new();
        for (client, _tx, amount) in &deposits {
            let amt_str = format!("{}.0000", amount);
            rows.push(CsvRow {
                tx_type: "deposit".to_string(),
                client: *client,
                tx: next_tx,
                amount: amt_str,
            });
            *expected.entry(*client).or_default() += *amount as f64;
            next_tx += 1;
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        for acc in &accounts {
            let total = parse_amount(&acc.total);
            let exp = expected.get(&acc.client).copied().unwrap_or(0.0);
            let diff = (total - exp).abs();
            prop_assert!(
                diff < 0.01,
                "client {}: expected total ~{}, got {}",
                acc.client, exp, acc.total
            );
            // No disputes, so held should be zero
            let held = parse_amount(&acc.held);
            prop_assert!(
                held.abs() < 0.00015,
                "client {}: expected zero held, got {}",
                acc.client, acc.held
            );
        }
    }

    /// Deposit then full withdrawal leaves zero balance.
    #[test]
    fn deposit_then_full_withdrawal_is_zero(amount in 1u64..=100_000u64) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount) },
            CsvRow { tx_type: "withdrawal".to_string(), client: 1, tx: 2, amount: format!("{}.0000", amount) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        prop_assert_eq!(accounts.len(), 1);
        prop_assert_eq!(&accounts[0].available, "0.0000");
        prop_assert_eq!(&accounts[0].held, "0.0000");
        prop_assert_eq!(&accounts[0].total, "0.0000");
        prop_assert_eq!(accounts[0].locked, false);
    }

    /// Deposit → dispute → resolve round-trip preserves original balance.
    #[test]
    fn dispute_resolve_preserves_balance(amount in 1u64..=100_000u64) {
        let amt = format!("{}.0000", amount);
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: amt.clone() },
            CsvRow { tx_type: "dispute".to_string(), client: 1, tx: 1, amount: amt.clone() },
            CsvRow { tx_type: "resolve".to_string(), client: 1, tx: 1, amount: amt.clone() },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        prop_assert_eq!(accounts.len(), 1);
        prop_assert_eq!(&accounts[0].available, &amt);
        prop_assert_eq!(&accounts[0].held, "0.0000");
        prop_assert_eq!(&accounts[0].total, &amt);
        prop_assert_eq!(accounts[0].locked, false);
    }

    /// Deposit → dispute → chargeback locks the account and removes the disputed amount.
    #[test]
    fn dispute_chargeback_locks_and_removes(
        deposit in 100u64..=100_000u64,
        dispute_frac in 1u32..=100u32,
    ) {
        let dispute_amt = (deposit * dispute_frac as u64) / 100;
        let dispute_amt = dispute_amt.max(1); // at least 1
        let remaining = deposit - dispute_amt;

        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            CsvRow { tx_type: "dispute".to_string(), client: 1, tx: 1, amount: format!("{}.0000", dispute_amt) },
            CsvRow { tx_type: "chargeback".to_string(), client: 1, tx: 1, amount: format!("{}.0000", dispute_amt) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        let expected_remaining = format!("{}.0000", remaining);
        prop_assert_eq!(accounts.len(), 1);
        prop_assert_eq!(&accounts[0].available, &expected_remaining);
        prop_assert_eq!(&accounts[0].held, "0.0000");
        prop_assert_eq!(&accounts[0].total, &expected_remaining);
        prop_assert!(accounts[0].locked, "account should be locked after chargeback");
    }

    /// After chargeback (locked), no further deposits or withdrawals change the balance.
    #[test]
    fn locked_account_rejects_mutations(
        deposit in 100u64..=10_000u64,
        extra_deposit in 1u64..=10_000u64,
        extra_withdrawal in 1u64..=10_000u64,
    ) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            CsvRow { tx_type: "dispute".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            CsvRow { tx_type: "chargeback".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            // These should all be rejected
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 2, amount: format!("{}.0000", extra_deposit) },
            CsvRow { tx_type: "withdrawal".to_string(), client: 1, tx: 3, amount: format!("{}.0000", extra_withdrawal) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        prop_assert_eq!(accounts.len(), 1);
        prop_assert_eq!(&accounts[0].available, "0.0000");
        prop_assert_eq!(&accounts[0].held, "0.0000");
        prop_assert_eq!(&accounts[0].total, "0.0000");
        prop_assert!(accounts[0].locked);
    }

    /// Withdrawal cannot exceed available balance — available stays non-negative.
    #[test]
    fn withdrawal_cannot_overdraw(
        deposit in 1u64..=10_000u64,
        withdrawal in 1u64..=20_000u64,
    ) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            CsvRow { tx_type: "withdrawal".to_string(), client: 1, tx: 2, amount: format!("{}.0000", withdrawal) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        let available = parse_amount(&accounts[0].available);
        prop_assert!(available >= -0.00005, "available went negative: {}", available);

        if withdrawal <= deposit {
            let expected = deposit - withdrawal;
            prop_assert_eq!(&accounts[0].available, &format!("{}.0000", expected));
        } else {
            // Withdrawal rejected, balance unchanged
            prop_assert_eq!(&accounts[0].available, &format!("{}.0000", deposit));
        }
    }

    /// Duplicate transaction IDs are rejected — second deposit with same tx ID is ignored.
    #[test]
    fn duplicate_tx_id_rejected(amount1 in 1u64..=10_000u64, amount2 in 1u64..=10_000u64) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount1) },
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount2) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        // Only the first deposit should count
        prop_assert_eq!(&accounts[0].total, &format!("{}.0000", amount1));
    }

    /// Disputing a non-existent transaction does not change any balances.
    #[test]
    fn dispute_nonexistent_tx_is_noop(deposit in 1u64..=10_000u64) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", deposit) },
            CsvRow { tx_type: "dispute".to_string(), client: 1, tx: 999, amount: String::new() },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        let expected = format!("{}.0000", deposit);
        prop_assert_eq!(&accounts[0].available, &expected);
        prop_assert_eq!(&accounts[0].held, "0.0000");
        prop_assert_eq!(&accounts[0].total, &expected);
    }

    /// Multiple clients are isolated — operations on one don't affect others.
    #[test]
    fn client_isolation(
        amount_a in 1u64..=10_000u64,
        amount_b in 1u64..=10_000u64,
    ) {
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount_a) },
            CsvRow { tx_type: "deposit".to_string(), client: 2, tx: 2, amount: format!("{}.0000", amount_b) },
            // Lock client 1
            CsvRow { tx_type: "dispute".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount_a) },
            CsvRow { tx_type: "chargeback".to_string(), client: 1, tx: 1, amount: format!("{}.0000", amount_a) },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        let acc_map: HashMap<u16, &OutputRow> = accounts.iter().map(|a| (a.client, a)).collect();

        // Client 1 locked with zero
        let c1 = acc_map[&1];
        prop_assert!(c1.locked);
        prop_assert_eq!(&c1.total, "0.0000");

        // Client 2 completely unaffected
        let c2 = acc_map[&2];
        prop_assert!(!c2.locked);
        prop_assert_eq!(&c2.total, &format!("{}.0000", amount_b));
        prop_assert_eq!(&c2.available, &format!("{}.0000", amount_b));
    }

    /// Precision: amounts with 4 decimal places survive round-trip exactly.
    #[test]
    fn four_decimal_precision_roundtrip(
        whole in 0u64..=99_999u64,
        frac in 0u32..=9999u32,
    ) {
        let amt = format!("{}.{:04}", whole, frac);
        let rows = vec![
            CsvRow { tx_type: "deposit".to_string(), client: 1, tx: 1, amount: amt.clone() },
        ];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let accounts = rt.block_on(run_engine(&rows)).unwrap();

        prop_assert_eq!(accounts.len(), 1);
        prop_assert_eq!(&accounts[0].available, &amt);
        prop_assert_eq!(&accounts[0].total, &amt);
    }
}
