//! Fuzz test: randomly generated transaction sequences against a single account.
//!
//! Every operation type (deposit, withdrawal, dispute, resolve, chargeback)
//! is chosen uniformly at random with random tx IDs and amounts.  After each
//! operation the exhaustive `AccountInvariantGuard` is triggered, catching any
//! balance, set-membership, or state-consistency violations.
//!
//! The test deliberately does NOT pre-seed deposits or structure the sequence
//! in any "realistic" way — the goal is to throw arbitrary garbage at the
//! account and verify it never panics and never violates an invariant.

use nda_takehome::domain::account::check_invariants;
use nda_takehome::domain::amount::Amount;
use nda_takehome::message::InputMessage;
use nda_takehome::{Account, TransactionType};
use proptest::prelude::*;
// ---------------------------------------------------------------------------
// Strategy: a single random operation
// ---------------------------------------------------------------------------

/// Represents one operation to apply to the account.
#[derive(Debug, Clone)]
struct Op {
    tx_type: TransactionType,
    tx_id: u32,
    amount: i64,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    (
        prop_oneof![
            Just(TransactionType::Deposit),
            Just(TransactionType::Withdrawal),
            Just(TransactionType::Dispute),
            Just(TransactionType::Resolve),
            Just(TransactionType::Chargeback),
        ],
        // Small tx ID space to maximise collisions / lifecycle interactions
        1u32..=10,
        // Amount range includes zero
        0i64..=1000,
    )
        .prop_map(|(tx_type, tx_id, amount)| Op {
            tx_type,
            tx_id,
            amount,
        })
}

/// Apply an operation and immediately check all invariants via the guard.
fn apply_op(acc: &mut Account, chrono: u64, op: &Op) {
    let msg = InputMessage {
        chrono_order: chrono,
        transaction_type: op.tx_type,
        client_id: 1,
        transaction_id: op.tx_id,
        amount: Amount::from_major(op.amount),
    };

    let _result = match op.tx_type {
        TransactionType::Deposit => acc.process_deposit(&msg),
        TransactionType::Withdrawal => acc.process_withdrawal(&msg),
        TransactionType::Dispute => acc.process_dispute(&msg),
        TransactionType::Resolve => acc.process_resolve(&msg),
        TransactionType::Chargeback => acc.process_chargeback(&msg),
    };
    // Result is intentionally ignored — errors are expected for invalid
    // sequences.  The invariant guard catches any actual corruption.
    //
    // The guard borrows `acc` immutably and runs the full invariant suite
    // when it drops at the end of this scope.
    check_invariants(acc);
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Throw a completely random sequence of operations at a single account.
    /// After every operation the exhaustive invariant guard fires.
    /// The test passes if nothing panics.
    #[test]
    fn random_ops_never_violate_invariants(
        ops in prop::collection::vec(op_strategy(), 1..100),
    ) {
        let mut acc = Account::new(1);
        for (i, op) in ops.iter().enumerate() {
            apply_op(&mut acc, i as u64, op);
        }
    }

    /// Same as above but with a much larger tx ID space — fewer collisions,
    /// more unique deposit/withdrawal records.
    #[test]
    fn random_ops_wide_tx_ids(
        ops in prop::collection::vec(
            (
                prop_oneof![
                    Just(TransactionType::Deposit),
                    Just(TransactionType::Withdrawal),
                    Just(TransactionType::Dispute),
                    Just(TransactionType::Resolve),
                    Just(TransactionType::Chargeback),
                ],
                1u32..=1000,
                0i64..=10_000,
            ).prop_map(|(tx_type, tx_id, amount)| Op { tx_type, tx_id, amount }),
            1..100,
        ),
    ) {
        let mut acc = Account::new(1);
        for (i, op) in ops.iter().enumerate() {
            apply_op(&mut acc, i as u64, op);
        }
    }

    /// Bias towards deposits first, then random lifecycle — still fully random
    /// but more likely to create a rich state to stress-test.
    #[test]
    fn deposit_heavy_then_random_lifecycle(
        deposits in prop::collection::vec(
            (1u32..=10, 1i64..=500),
            1..15,
        ),
        lifecycle in prop::collection::vec(
            (
                prop_oneof![
                    Just(TransactionType::Dispute),
                    Just(TransactionType::Resolve),
                    Just(TransactionType::Chargeback),
                    Just(TransactionType::Withdrawal),
                    Just(TransactionType::Deposit),
                ],
                1u32..=10,
                0i64..=1000,
            ).prop_map(|(tx_type, tx_id, amount)| Op { tx_type, tx_id, amount }),
            0..50,
        ),
    ) {
        let mut acc = Account::new(1);
        let mut chrono = 0u64;

        for &(tx_id, amount) in &deposits {
            apply_op(&mut acc, chrono, &Op {
                tx_type: TransactionType::Deposit,
                tx_id,
                amount,
            });
            chrono += 1;
        }

        for op in &lifecycle {
            apply_op(&mut acc, chrono, op);
            chrono += 1;
        }
    }

    /// Zero-amount operations should never corrupt state.
    #[test]
    fn zero_amount_ops_are_safe(
        ops in prop::collection::vec(
            (
                prop_oneof![
                    Just(TransactionType::Deposit),
                    Just(TransactionType::Withdrawal),
                    Just(TransactionType::Dispute),
                    Just(TransactionType::Resolve),
                    Just(TransactionType::Chargeback),
                ],
                1u32..=5,
            ).prop_map(|(tx_type, tx_id)| Op { tx_type, tx_id, amount: 0 }),
            1..50,
        ),
    ) {
        let mut acc = Account::new(1);
        for (i, op) in ops.iter().enumerate() {
            apply_op(&mut acc, i as u64, op);
        }
    }

    /// Hammer a single tx ID with every operation type to maximise
    /// state-transition coverage on one record.
    #[test]
    fn single_tx_id_hammered(
        seed_amount in 1i64..=1000,
        ops in prop::collection::vec(
            (
                prop_oneof![
                    Just(TransactionType::Deposit),
                    Just(TransactionType::Withdrawal),
                    Just(TransactionType::Dispute),
                    Just(TransactionType::Resolve),
                    Just(TransactionType::Chargeback),
                ],
                0i64..=500,
            ),
            1..80,
        ),
    ) {
        let mut acc = Account::new(1);
        apply_op(&mut acc, 0, &Op {
            tx_type: TransactionType::Deposit,
            tx_id: 1,
            amount: seed_amount,
        });

        for (i, &(tx_type, amount)) in ops.iter().enumerate() {
            apply_op(&mut acc, (i + 1) as u64, &Op {
                tx_type,
                tx_id: 1,
                amount,
            });
        }
    }
}
