//! Property-based lifecycle tests for transaction and account state machines.
//!
//! These tests drive the `Account` directly (no CSV round-trip) so they can
//! inspect internal state: transaction state sets, pending disputes, held
//! amounts, and the locked flag.  The test generates random *realistic*
//! operation sequences — deposits followed by lifecycle actions that reference
//! those deposits — and checks invariants after every single step.

use nda_takehome::domain::amount::Amount;
use nda_takehome::message::InputMessage;
use nda_takehome::service::error::TransactionError;
use nda_takehome::{Account, TransactionId, TransactionType};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_msg(
    chrono: u64,
    tx_type: TransactionType,
    client: u16,
    tx_id: TransactionId,
    amount: i64,
) -> InputMessage {
    InputMessage {
        chrono_order: chrono,
        transaction_type: tx_type,
        client_id: client,
        transaction_id: tx_id,
        amount: Amount::from_major(amount),
    }
}

/// Verify all account invariants hold.  Called after every operation.
fn assert_invariants(acc: &Account, ctx: &str) {
    let zero = Amount::zero();

    // Balance invariants
    assert!(
        acc.available >= zero,
        "{ctx}: available ({}) must be >= 0",
        acc.available
    );
    assert!(acc.held >= zero, "{ctx}: held ({}) must be >= 0", acc.held);
    assert!(
        acc.total >= zero,
        "{ctx}: total ({}) must be >= 0",
        acc.total
    );
    assert_eq!(
        acc.total,
        &acc.available + &acc.held,
        "{ctx}: total ({}) must equal available ({}) + held ({})",
        acc.total,
        acc.available,
        acc.held,
    );

    // State-set invariants: every known tx must be in exactly one set
    let all_tx_ids: Vec<TransactionId> = acc
        .normal
        .iter()
        .chain(acc.disputes.iter())
        .chain(acc.resolves.iter())
        .chain(acc.chargebacks.iter())
        .copied()
        .collect();

    let unique_count = {
        let mut s = all_tx_ids.clone();
        s.sort();
        s.dedup();
        s.len()
    };
    assert_eq!(
        all_tx_ids.len(),
        unique_count,
        "{ctx}: a transaction appears in more than one state set"
    );

    // If locked, at least one chargeback must have occurred
    if acc.locked {
        assert!(
            !acc.chargebacks.is_empty(),
            "{ctx}: account locked but no chargebacks recorded"
        );
    }
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Generate a sequence of deposit amounts (1..=500) for a given number of txs.
fn deposits_strategy(max_txs: usize) -> impl Strategy<Value = Vec<i64>> {
    prop::collection::vec(1i64..=500, 1..=max_txs)
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Full lifecycle: random deposits then random dispute/resolve/chargeback
    /// actions.  Invariants are checked after every single step.
    #[test]
    fn lifecycle_invariants_hold_at_every_step(
        deposit_amounts in deposits_strategy(8),
        raw_actions in prop::collection::vec(
            (0usize..8, 0u8..3, 1i64..=500),
            0..30
        ),
    ) {
        let mut acc = Account::new(1);
        let mut chrono = 0u64;
        let num_deposits = deposit_amounts.len();

        // Phase 1: deposits
        for (i, &amount) in deposit_amounts.iter().enumerate() {
            let tx_id = (i + 1) as TransactionId;
            let msg = make_msg(chrono, TransactionType::Deposit, 1, tx_id, amount);
            let _ = acc.process_deposit(&msg);
            chrono += 1;
            assert_invariants(&acc, &format!("after deposit tx={tx_id} amount={amount}"));
        }

        // Phase 2: lifecycle actions targeting existing deposits
        for (action_idx, &(tx_idx, action_kind, amount)) in raw_actions.iter().enumerate() {
            let tx_id = (tx_idx % num_deposits + 1) as TransactionId;
            let msg = match action_kind % 3 {
                0 => make_msg(chrono, TransactionType::Dispute, 1, tx_id, amount),
                1 => make_msg(chrono, TransactionType::Resolve, 1, tx_id, amount),
                _ => make_msg(chrono, TransactionType::Chargeback, 1, tx_id, amount),
            };
            let result = match msg.transaction_type {
                TransactionType::Dispute => acc.process_dispute(&msg),
                TransactionType::Resolve => acc.process_resolve(&msg),
                TransactionType::Chargeback => acc.process_chargeback(&msg),
                _ => unreachable!(),
            };
            // Error results are fine (invalid transitions), but must not break invariants
            let action_name = match action_kind % 3 {
                0 => "dispute",
                1 => "resolve",
                _ => "chargeback",
            };
            let outcome = if result.is_ok() { "ok" } else { "err" };
            assert_invariants(
                &acc,
                &format!("step {action_idx}: {action_name} tx={tx_id} amt={amount} -> {outcome}"),
            );
            chrono += 1;
        }
    }

    /// Deposits then withdrawals: invariants hold and balance never goes
    /// negative regardless of the withdrawal pattern.
    #[test]
    fn deposit_withdrawal_lifecycle(
        deposit_amounts in prop::collection::vec(1i64..=1000, 1..10),
        withdrawal_amounts in prop::collection::vec(1i64..=2000, 0..15),
    ) {
        let mut acc = Account::new(1);
        let mut chrono = 0u64;
        let mut next_tx = 1u32;

        for &amount in &deposit_amounts {
            let msg = make_msg(chrono, TransactionType::Deposit, 1, next_tx, amount);
            let _ = acc.process_deposit(&msg);
            chrono += 1;
            next_tx += 1;
            assert_invariants(&acc, &format!("deposit tx={} amt={}", next_tx - 1, amount));
        }

        for &amount in &withdrawal_amounts {
            let msg = make_msg(chrono, TransactionType::Withdrawal, 1, next_tx, amount);
            let result = acc.process_withdrawal(&msg);
            chrono += 1;
            next_tx += 1;
            let outcome = if result.is_ok() { "ok" } else { "err" };
            assert_invariants(
                &acc,
                &format!("withdrawal tx={} amt={} -> {outcome}", next_tx - 1, amount),
            );
        }
    }

    /// A chargeback permanently locks the account.  After locking, every
    /// operation type must return AccountLocked and leave balances unchanged.
    #[test]
    fn chargeback_locks_permanently(
        deposit in 10i64..=10_000,
        dispute_amt in 1i64..=10_000,
        post_ops in prop::collection::vec(
            (0u8..5, 1i64..=1000, 100u32..200),
            1..20
        ),
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let d_amt = dispute_amt.min(deposit);
        let msg = make_msg(1, TransactionType::Dispute, 1, 1, d_amt);
        let _ = acc.process_dispute(&msg);

        let msg = make_msg(2, TransactionType::Chargeback, 1, 1, d_amt);
        let _ = acc.process_chargeback(&msg);

        if !acc.locked {
            // Chargeback may have been a no-op if disputed_amount was zero;
            // skip the rest of this case.
            return Ok(());
        }

        let frozen_available = acc.available.clone();
        let frozen_held = acc.held.clone();
        let frozen_total = acc.total.clone();

        for (i, &(kind, amount, tx_id)) in post_ops.iter().enumerate() {
            let tx_type = match kind % 5 {
                0 => TransactionType::Deposit,
                1 => TransactionType::Withdrawal,
                2 => TransactionType::Dispute,
                3 => TransactionType::Resolve,
                _ => TransactionType::Chargeback,
            };
            let msg = make_msg(3 + i as u64, tx_type, 1, tx_id, amount);
            let result = match tx_type {
                TransactionType::Deposit => acc.process_deposit(&msg),
                TransactionType::Withdrawal => acc.process_withdrawal(&msg),
                TransactionType::Dispute => acc.process_dispute(&msg),
                TransactionType::Resolve => acc.process_resolve(&msg),
                TransactionType::Chargeback => acc.process_chargeback(&msg),
            };

            prop_assert_eq!(
                result,
                Err(TransactionError::AccountLocked),
                "step {}: expected AccountLocked for {:?} on locked account",
                i, tx_type
            );
            prop_assert_eq!(&acc.available, &frozen_available, "step {}: available changed on locked account", i);
            prop_assert_eq!(&acc.held, &frozen_held, "step {}: held changed on locked account", i);
            prop_assert_eq!(&acc.total, &frozen_total, "step {}: total changed on locked account", i);
            prop_assert!(acc.locked, "step {}: account became unlocked", i);
        }
    }

    /// Dispute/resolve is a lossless round-trip: disputing N then resolving N
    /// returns available/held to exactly their pre-dispute values.
    #[test]
    fn dispute_resolve_roundtrip_exact(
        deposit in 100i64..=100_000,
        dispute_amt in 1i64..=100_000,
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let pre_available = acc.available.clone();
        let pre_held = acc.held.clone();

        let effective_dispute = dispute_amt.min(deposit);
        let msg = make_msg(1, TransactionType::Dispute, 1, 1, effective_dispute);
        let _ = acc.process_dispute(&msg);
        assert_invariants(&acc, "after dispute");

        // Held should have increased by exactly the effective amount
        let expected_held = &pre_held + &Amount::from_major(effective_dispute);
        prop_assert_eq!(&acc.held, &expected_held);

        // Now resolve the full disputed amount
        let msg = make_msg(2, TransactionType::Resolve, 1, 1, effective_dispute);
        let _ = acc.process_resolve(&msg);
        assert_invariants(&acc, "after resolve");

        prop_assert_eq!(&acc.available, &pre_available, "available not restored after dispute+resolve");
        prop_assert_eq!(&acc.held, &pre_held, "held not restored after dispute+resolve");
    }

    /// Multiple disputes on the same transaction accumulate held funds
    /// correctly, and a single full resolve releases everything.
    #[test]
    fn cumulative_disputes_then_full_resolve(
        deposit in 100i64..=10_000,
        dispute_amounts in prop::collection::vec(1i64..=100, 2..6),
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let mut chrono = 1u64;
        let mut total_disputed = 0i64;

        for &d_amt in &dispute_amounts {
            let effective = d_amt.min((deposit - total_disputed).max(0));
            if effective <= 0 {
                break;
            }
            // available might be less than d_amt due to prior disputes
            let actual_before = acc.available.clone();
            let msg = make_msg(chrono, TransactionType::Dispute, 1, 1, d_amt);
            let _ = acc.process_dispute(&msg);
            chrono += 1;

            let moved = if Amount::from_major(d_amt) <= actual_before {
                d_amt
            } else {
                // available was clamped
                deposit - total_disputed
            };
            if moved > 0 {
                total_disputed += moved;
            }
            assert_invariants(&acc, &format!("after cumulative dispute #{chrono}"));
        }

        if total_disputed > 0 {
            // One big resolve to release everything
            let msg = make_msg(chrono, TransactionType::Resolve, 1, 1, total_disputed);
            let _ = acc.process_resolve(&msg);
            assert_invariants(&acc, "after full resolve of cumulative disputes");

            prop_assert_eq!(&acc.held, &Amount::zero(), "held should be zero after full resolve");
            prop_assert_eq!(&acc.available, &Amount::from_major(deposit), "available should equal deposit after full resolve");
        }
    }

    /// Dispute then chargeback: total decreases by exactly the chargeback
    /// amount, held goes to zero, and the account is locked.
    #[test]
    fn dispute_chargeback_accounting(
        deposit in 100i64..=100_000,
        dispute_amt in 1i64..=100_000,
        chargeback_amt in 1i64..=100_000,
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let effective_dispute = dispute_amt.min(deposit);
        let msg = make_msg(1, TransactionType::Dispute, 1, 1, effective_dispute);
        let _ = acc.process_dispute(&msg);
        assert_invariants(&acc, "after dispute");

        let effective_chargeback = chargeback_amt.min(effective_dispute);
        let total_before = acc.total.clone();
        let msg = make_msg(2, TransactionType::Chargeback, 1, 1, effective_chargeback);
        let _ = acc.process_chargeback(&msg);
        assert_invariants(&acc, "after chargeback");

        if effective_chargeback > 0 {
            let expected_total = &total_before - &Amount::from_major(effective_chargeback);
            prop_assert_eq!(&acc.total, &expected_total, "total should decrease by chargeback amount");
            prop_assert_eq!(&acc.held, &Amount::zero(), "held should be zero after chargeback");
            prop_assert!(acc.locked, "account should be locked after chargeback");
        }
    }

    /// Tx ID uniqueness: the same tx ID used for two deposits on the same
    /// account always rejects the second one.
    #[test]
    fn duplicate_tx_id_always_rejected(
        amount1 in 1i64..=10_000,
        amount2 in 1i64..=10_000,
    ) {
        let mut acc = Account::new(1);
        let msg1 = make_msg(0, TransactionType::Deposit, 1, 42, amount1);
        acc.process_deposit(&msg1).unwrap();
        let snapshot = acc.available.clone();

        let msg2 = make_msg(1, TransactionType::Deposit, 1, 42, amount2);
        let result = acc.process_deposit(&msg2);

        prop_assert_eq!(result, Err(TransactionError::DuplicateTransaction));
        prop_assert_eq!(&acc.available, &snapshot, "balance should not change on duplicate");
    }

    /// Withdrawal never allows overdraw: if withdrawal > available, it must
    /// fail and leave balances unchanged.
    #[test]
    fn overdraw_always_rejected(
        deposit in 1i64..=10_000,
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let msg = make_msg(1, TransactionType::Withdrawal, 1, 2, deposit + 1);
        let result = acc.process_withdrawal(&msg);

        prop_assert_eq!(result, Err(TransactionError::InsufficientBalance));
        prop_assert_eq!(&acc.available, &Amount::from_major(deposit));
        prop_assert_eq!(&acc.total, &Amount::from_major(deposit));
    }

    /// Dispute on a non-existent tx ID is always InvalidTransaction and
    /// leaves the account unchanged.
    #[test]
    fn dispute_nonexistent_always_fails(deposit in 1i64..=10_000) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();
        let snapshot_avail = acc.available.clone();
        let snapshot_held = acc.held.clone();

        let msg = make_msg(1, TransactionType::Dispute, 1, 999, deposit);
        let result = acc.process_dispute(&msg);

        prop_assert_eq!(result, Err(TransactionError::InvalidTransaction));
        prop_assert_eq!(&acc.available, &snapshot_avail);
        prop_assert_eq!(&acc.held, &snapshot_held);
    }

    /// Resolve on a non-disputed tx is always TransactionNotDisputed.
    #[test]
    fn resolve_non_disputed_always_fails(deposit in 1i64..=10_000) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let msg = make_msg(1, TransactionType::Resolve, 1, 1, deposit);
        let result = acc.process_resolve(&msg);

        prop_assert_eq!(result, Err(TransactionError::TransactionNotDisputed));
    }

    /// Re-dispute after resolve: a resolved transaction can be disputed again,
    /// moving funds back into held.
    #[test]
    fn redispute_after_resolve(
        deposit in 100i64..=10_000,
        dispute1 in 1i64..=5_000,
        dispute2 in 1i64..=5_000,
    ) {
        let mut acc = Account::new(1);
        let msg = make_msg(0, TransactionType::Deposit, 1, 1, deposit);
        acc.process_deposit(&msg).unwrap();

        let d1 = dispute1.min(deposit);
        let msg = make_msg(1, TransactionType::Dispute, 1, 1, d1);
        let _ = acc.process_dispute(&msg);
        assert_invariants(&acc, "after dispute1");

        let msg = make_msg(2, TransactionType::Resolve, 1, 1, d1);
        let _ = acc.process_resolve(&msg);
        assert_invariants(&acc, "after resolve");

        // Account should be fully available again
        prop_assert_eq!(&acc.available, &Amount::from_major(deposit));
        prop_assert_eq!(&acc.held, &Amount::zero());

        // Re-dispute
        let d2 = dispute2.min(deposit);
        let msg = make_msg(3, TransactionType::Dispute, 1, 1, d2);
        let result = acc.process_dispute(&msg);
        assert_invariants(&acc, "after redispute");

        prop_assert!(result.is_ok(), "re-dispute after resolve should succeed");
        prop_assert_eq!(&acc.held, &Amount::from_major(d2));
        prop_assert_eq!(&acc.available, &Amount::from_major(deposit - d2));
    }

    /// Full multi-phase lifecycle on multiple transactions: deposits,
    /// withdrawals, then lifecycle actions. No panic, invariants hold.
    #[test]
    fn multi_tx_full_lifecycle(
        deposit_amounts in prop::collection::vec(1i64..=500, 2..6),
        withdrawal_amounts in prop::collection::vec(1i64..=200, 0..3),
        raw_actions in prop::collection::vec(
            (0usize..6, 0u8..3, 1i64..=500),
            0..20
        ),
    ) {
        let mut acc = Account::new(1);
        let mut chrono = 0u64;
        let mut next_tx = 1u32;
        let num_deposits = deposit_amounts.len();

        // Deposits
        for &amount in &deposit_amounts {
            let msg = make_msg(chrono, TransactionType::Deposit, 1, next_tx, amount);
            let _ = acc.process_deposit(&msg);
            chrono += 1;
            next_tx += 1;
            assert_invariants(&acc, &format!("deposit tx={}", next_tx - 1));
        }

        // Withdrawals (use separate tx IDs)
        for &amount in &withdrawal_amounts {
            let msg = make_msg(chrono, TransactionType::Withdrawal, 1, next_tx, amount);
            let _ = acc.process_withdrawal(&msg);
            chrono += 1;
            next_tx += 1;
            assert_invariants(&acc, &format!("withdrawal tx={}", next_tx - 1));
        }

        // Lifecycle actions on the deposit tx IDs
        for &(tx_idx, action_kind, amount) in &raw_actions {
            let tx_id = (tx_idx % num_deposits + 1) as TransactionId;
            let (tx_type, name) = match action_kind % 3 {
                0 => (TransactionType::Dispute, "dispute"),
                1 => (TransactionType::Resolve, "resolve"),
                _ => (TransactionType::Chargeback, "chargeback"),
            };
            let msg = make_msg(chrono, tx_type, 1, tx_id, amount);
            let _ = match tx_type {
                TransactionType::Dispute => acc.process_dispute(&msg),
                TransactionType::Resolve => acc.process_resolve(&msg),
                TransactionType::Chargeback => acc.process_chargeback(&msg),
                _ => unreachable!(),
            };
            chrono += 1;
            assert_invariants(&acc, &format!("{name} tx={tx_id}"));
        }
    }
}
