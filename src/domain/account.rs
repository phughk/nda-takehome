use std::collections::{HashMap, HashSet};

use crate::domain::amount::Amount;
use crate::domain::ClientId;
use crate::service::error::TransactionError;
use crate::{InputMessage, TransactionId};

/// A client account that tracks balances and transaction lifecycle states.
#[derive(Debug, Default, PartialEq)]
pub struct Account {
    /// Unique client identifier.
    pub client_id: ClientId,
    /// Funds available for withdrawal.
    pub available: Amount,
    /// Funds held due to pending disputes.
    pub held: Amount,
    /// Total funds (`available + held`).
    pub total: Amount,
    /// Whether the account is frozen after a chargeback.
    pub locked: bool,
    /// All recorded transactions keyed by ID, with their current state.
    pub(crate) transactions: HashMap<TransactionId, (TransactionState, InputMessage)>,
    /// Transaction IDs in the `Normal` state.
    pub normal: HashSet<TransactionId>,
    /// Transaction IDs in the `Disputed` state.
    pub disputes: HashSet<TransactionId>,
    /// Transaction IDs in the `Resolved` state.
    pub resolves: HashSet<TransactionId>,
    /// Transaction IDs in the `Chargeback` state.
    pub chargebacks: HashSet<TransactionId>,
    /// Amounts currently held per disputed transaction.
    pub pending_disputes: HashMap<TransactionId, Amount>,
}

/// The lifecycle state of a transaction within an account.
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub(crate) enum TransactionState {
    /// Initial state after deposit or withdrawal.
    #[default]
    Normal,
    /// Transaction is under dispute; funds moved from available to held.
    Disputed,
    /// Dispute has been resolved; funds returned to available.
    Resolved,
    /// Dispute resulted in a chargeback; account is locked.
    Chargeback,
}

impl TransactionState {
    /// Validates whether a state transition is allowed, returning the transition
    /// descriptor on success or an error explaining why it was rejected.
    fn can_transition(
        self,
        target: TransactionState,
    ) -> Result<ExecuteTransition, TransactionError> {
        /*
        I considered a few things around this implementation

        1) Using structs and generics for transitions makes a mess around using Sized, and
        dynamic dispatch is unfavourable. Therefore, enums are a better choice for a general
        purpose state machine that is stored in a vec.

        2) Using properties (ex. "fn must_be_locked(&self) -> bool) instead of matching 2 states
        means that cases might be missed. We don't want to miss cases, so properties of enums
        are inconvenient.
         */
        match (self, target) {
            // No transition can target Normal
            (_, TransactionState::Normal) => Err(TransactionError::CannotRevertToNormal),
            // Chargeback is a terminal state
            (TransactionState::Chargeback, _) => Err(TransactionError::AccountLocked),
            // Dispute: from Normal, Disputed (cumulative), or Resolved (re-dispute)
            (
                TransactionState::Normal | TransactionState::Disputed | TransactionState::Resolved,
                TransactionState::Disputed,
            ) => Ok(ExecuteTransition {
                from: self,
                to: target,
            }),
            // Resolve / Chargeback: only from Disputed
            (
                TransactionState::Disputed,
                TransactionState::Resolved | TransactionState::Chargeback,
            ) => Ok(ExecuteTransition {
                from: self,
                to: target,
            }),
            // Cannot resolve or chargeback a transaction that is not disputed
            (
                TransactionState::Normal,
                TransactionState::Resolved | TransactionState::Chargeback,
            ) => Err(TransactionError::TransactionNotDisputed),
            (
                TransactionState::Resolved,
                TransactionState::Resolved | TransactionState::Chargeback,
            ) => Err(TransactionError::TransactionNotDisputed),
        }
    }
}

/// Describes a validated state transition to be applied to an account.
struct ExecuteTransition {
    /// State the transaction is moving from.
    from: TransactionState,
    /// State the transaction is moving to.
    to: TransactionState,
}

impl ExecuteTransition {
    /// Applies the state transition: removes the transaction from its old state set,
    /// inserts it into the new one, and updates the transactions map.
    fn execute(self, acc: &mut Account, key: TransactionId) {
        match self.from {
            TransactionState::Normal => {
                acc.normal.remove(&key);
            }
            TransactionState::Disputed => {
                acc.disputes.remove(&key);
            }
            TransactionState::Resolved => {
                acc.resolves.remove(&key);
            }
            TransactionState::Chargeback => {
                acc.chargebacks.remove(&key);
            }
        }
        match self.to {
            TransactionState::Normal => {
                acc.normal.insert(key);
            }
            TransactionState::Disputed => {
                acc.disputes.insert(key);
            }
            TransactionState::Resolved => {
                acc.resolves.insert(key);
            }
            TransactionState::Chargeback => {
                acc.chargebacks.insert(key);
            }
        }
        if let Some((state, _)) = acc.transactions.get_mut(&key) {
            *state = self.to;
        }
    }
}

impl Account {
    /// Creates a new account with zero balances for the given client.
    pub fn new(client_id: ClientId) -> Self {
        Self {
            client_id,
            ..Default::default()
        }
    }

    /// Returns an invariant guard that checks all account invariants on drop (debug builds only).
    pub fn invariant_guard(&self) -> AccountInvariantGuard<'_> {
        AccountInvariantGuard::new(self)
    }

    /// Processes a deposit: adds funds to available and total.
    /// Rejects duplicate transaction IDs and operations on locked accounts.
    pub fn process_deposit(&mut self, msg: &InputMessage) -> Result<(), TransactionError> {
        if self.locked {
            return Err(TransactionError::AccountLocked);
        }
        if self.transactions.contains_key(&msg.transaction_id) {
            return Err(TransactionError::DuplicateTransaction);
        }
        self.transactions
            .insert(msg.transaction_id, (TransactionState::Normal, msg.clone()));
        self.normal.insert(msg.transaction_id);
        self.available += &msg.amount;
        self.total += &msg.amount;
        Ok(())
    }

    /// Processes a withdrawal: subtracts funds from available and total.
    /// Rejects if insufficient balance, duplicate transaction, or account locked.
    pub fn process_withdrawal(&mut self, msg: &InputMessage) -> Result<(), TransactionError> {
        if self.locked {
            return Err(TransactionError::AccountLocked);
        }
        if self.transactions.contains_key(&msg.transaction_id) {
            return Err(TransactionError::DuplicateTransaction);
        }
        if self.available < msg.amount {
            return Err(TransactionError::InsufficientBalance);
        }
        self.transactions
            .insert(msg.transaction_id, (TransactionState::Normal, msg.clone()));
        self.normal.insert(msg.transaction_id);
        self.available -= &msg.amount;
        self.total -= &msg.amount;
        Ok(())
    }

    /// Processes a dispute: moves funds from available to held (capped at available balance).
    /// A zero-amount dispute is a no-op. Cumulative disputes on the same transaction are allowed.
    pub fn process_dispute(&mut self, msg: &InputMessage) -> Result<(), TransactionError> {
        if self.locked {
            return Err(TransactionError::AccountLocked);
        }
        let key = msg.transaction_id;
        let transition = self.do_transition(key, TransactionState::Disputed)?;
        let disputed_amount = if msg.amount <= self.available {
            msg.amount.clone()
        } else {
            self.available.clone()
        };
        if disputed_amount.is_positive() {
            self.available -= &disputed_amount;
            self.held += &disputed_amount;
            *self.pending_disputes.entry(key).or_insert(Amount::zero()) += &disputed_amount;
            transition.execute(self, key);
        }
        Ok(())
    }

    /// Processes a resolve: returns held funds back to available.
    /// A partial resolve keeps the transaction in Disputed state with the remaining held amount.
    pub fn process_resolve(&mut self, msg: &InputMessage) -> Result<(), TransactionError> {
        if self.locked {
            return Err(TransactionError::AccountLocked);
        }
        let key = msg.transaction_id;
        let transition = self.do_transition(key, TransactionState::Resolved)?;
        let disputed = self
            .pending_disputes
            .get(&key)
            .ok_or(TransactionError::TransactionNotDisputed)?
            .clone();
        let resolve_amount = if msg.amount < disputed {
            msg.amount.clone()
        } else {
            disputed.clone()
        };
        if resolve_amount.is_positive() {
            self.held -= &resolve_amount;
            self.available += &resolve_amount;
            let remaining = &disputed - &resolve_amount;
            if remaining.is_positive() {
                self.pending_disputes.insert(key, remaining);
                // Partial resolve: state stays Disputed
            } else {
                self.pending_disputes.remove(&key);
                transition.execute(self, key);
            }
        }
        Ok(())
    }

    /// Processes a chargeback: removes held funds from total and locks the account.
    /// Any remaining disputed amount beyond the chargeback is returned to available.
    pub fn process_chargeback(&mut self, msg: &InputMessage) -> Result<(), TransactionError> {
        if self.locked {
            return Err(TransactionError::AccountLocked);
        }
        let key = msg.transaction_id;
        let transition = self.do_transition(key, TransactionState::Chargeback)?;
        let disputed = self
            .pending_disputes
            .get(&key)
            .ok_or(TransactionError::TransactionNotDisputed)?
            .clone();
        let chargeback_amount = if msg.amount < disputed {
            msg.amount.clone()
        } else {
            disputed.clone()
        };
        if chargeback_amount.is_positive() {
            let remaining_disputed = &disputed - &chargeback_amount;
            self.held -= &disputed;
            self.total -= &chargeback_amount;
            self.available += &remaining_disputed;
            self.locked = true;
            self.pending_disputes.remove(&key);
            transition.execute(self, key);
        }
        Ok(())
    }

    /// Looks up a transaction and validates whether it can transition to `target`.
    fn do_transition(
        &self,
        tx_id: TransactionId,
        target: TransactionState,
    ) -> Result<ExecuteTransition, TransactionError> {
        let (state, _) = self
            .transactions
            .get(&tx_id)
            .ok_or(TransactionError::InvalidTransaction)?;
        state.can_transition(target)
    }
}

/// Zero-sized in release builds. In debug builds holds a reference to the account
/// and checks all invariants on drop, panicking if any are violated.
pub struct AccountInvariantGuard<'a> {
    #[cfg(debug_assertions)]
    account: &'a Account,
    #[cfg(not(debug_assertions))]
    _phantom: std::marker::PhantomData<&'a Account>,
}

impl<'a> AccountInvariantGuard<'a> {
    /// Creates a new invariant guard. In debug builds, stores a reference to the account
    /// so all invariants can be checked when the guard is dropped.
    pub fn new(account: &'a Account) -> Self {
        Self {
            #[cfg(debug_assertions)]
            account,
            #[cfg(not(debug_assertions))]
            _phantom: std::marker::PhantomData,
        }
    }
}

impl Drop for AccountInvariantGuard<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        {
            let a = self.account;
            let zero = Amount::zero();

            // ── Balance invariants ──────────────────────────────────────
            assert!(
                a.available >= zero,
                "available must be >= 0, got {}",
                a.available
            );
            assert!(a.held >= zero, "held must be >= 0, got {}", a.held);
            assert!(a.total >= zero, "total must be >= 0, got {}", a.total);
            assert_eq!(
                a.total,
                &a.available + &a.held,
                "total ({}) must equal available ({}) + held ({})",
                a.total,
                a.available,
                a.held,
            );

            // ── Set-membership invariants ───────────────────────────────
            // Every transaction in the transactions map must appear in
            // exactly one of the four state sets.
            for (&tx_id, (state, _)) in &a.transactions {
                let in_normal = a.normal.contains(&tx_id);
                let in_disputes = a.disputes.contains(&tx_id);
                let in_resolves = a.resolves.contains(&tx_id);
                let in_chargebacks = a.chargebacks.contains(&tx_id);

                let count =
                    in_normal as u8 + in_disputes as u8 + in_resolves as u8 + in_chargebacks as u8;

                assert_eq!(
                    count, 1,
                    "tx {} is in {} state sets (normal={}, disputes={}, resolves={}, chargebacks={}), expected exactly 1",
                    tx_id, count, in_normal, in_disputes, in_resolves, in_chargebacks,
                );

                // The set the tx is in must match its recorded state.
                let expected_set = match state {
                    TransactionState::Normal => in_normal,
                    TransactionState::Disputed => in_disputes,
                    TransactionState::Resolved => in_resolves,
                    TransactionState::Chargeback => in_chargebacks,
                };
                assert!(
                    expected_set,
                    "tx {} has state {:?} but is not in the matching set (normal={}, disputes={}, resolves={}, chargebacks={})",
                    tx_id, state, in_normal, in_disputes, in_resolves, in_chargebacks,
                );
            }

            // Every entry in each state set must exist in the transactions map.
            for &tx_id in &a.normal {
                assert!(
                    a.transactions.contains_key(&tx_id),
                    "tx {} in normal set but missing from transactions map",
                    tx_id,
                );
            }
            for &tx_id in &a.disputes {
                assert!(
                    a.transactions.contains_key(&tx_id),
                    "tx {} in disputes set but missing from transactions map",
                    tx_id,
                );
            }
            for &tx_id in &a.resolves {
                assert!(
                    a.transactions.contains_key(&tx_id),
                    "tx {} in resolves set but missing from transactions map",
                    tx_id,
                );
            }
            for &tx_id in &a.chargebacks {
                assert!(
                    a.transactions.contains_key(&tx_id),
                    "tx {} in chargebacks set but missing from transactions map",
                    tx_id,
                );
            }

            // The union of the four sets must have the same size as the
            // transactions map — no orphans in either direction.
            let set_total =
                a.normal.len() + a.disputes.len() + a.resolves.len() + a.chargebacks.len();
            assert_eq!(
                set_total,
                a.transactions.len(),
                "state sets total ({}) != transactions map size ({})",
                set_total,
                a.transactions.len(),
            );

            // No overlap between sets (belt-and-suspenders — the per-tx
            // check above covers this, but this catches set-level issues).
            assert!(
                a.normal.is_disjoint(&a.disputes),
                "normal and disputes sets overlap: {:?}",
                a.normal.intersection(&a.disputes).collect::<Vec<_>>(),
            );
            assert!(
                a.normal.is_disjoint(&a.resolves),
                "normal and resolves sets overlap: {:?}",
                a.normal.intersection(&a.resolves).collect::<Vec<_>>(),
            );
            assert!(
                a.normal.is_disjoint(&a.chargebacks),
                "normal and chargebacks sets overlap: {:?}",
                a.normal.intersection(&a.chargebacks).collect::<Vec<_>>(),
            );
            assert!(
                a.disputes.is_disjoint(&a.resolves),
                "disputes and resolves sets overlap: {:?}",
                a.disputes.intersection(&a.resolves).collect::<Vec<_>>(),
            );
            assert!(
                a.disputes.is_disjoint(&a.chargebacks),
                "disputes and chargebacks sets overlap: {:?}",
                a.disputes.intersection(&a.chargebacks).collect::<Vec<_>>(),
            );
            assert!(
                a.resolves.is_disjoint(&a.chargebacks),
                "resolves and chargebacks sets overlap: {:?}",
                a.resolves.intersection(&a.chargebacks).collect::<Vec<_>>(),
            );

            // ── Pending-disputes invariants ─────────────────────────────
            // Every pending_dispute key must be in the disputes set.
            for (&tx_id, amount) in &a.pending_disputes {
                assert!(
                    a.disputes.contains(&tx_id),
                    "pending_dispute for tx {} but tx not in disputes set",
                    tx_id,
                );
                assert!(
                    *amount > zero,
                    "pending_dispute for tx {} has non-positive amount {}",
                    tx_id,
                    amount,
                );
            }

            // Every disputed tx must have a pending_disputes entry (disputes
            // only enter the set when a positive amount is moved to held).
            for &tx_id in &a.disputes {
                assert!(
                    a.pending_disputes.contains_key(&tx_id),
                    "tx {} in disputes set but has no pending_disputes entry",
                    tx_id,
                );
            }

            // ── Locked-account invariant ────────────────────────────────
            // If the account is locked, at least one chargeback must exist.
            if a.locked {
                assert!(
                    !a.chargebacks.is_empty(),
                    "account is locked but no chargebacks recorded",
                );
            }

            // ── Held-amount consistency ─────────────────────────────────
            // The sum of all pending_disputes amounts must equal held.
            let pending_sum = a
                .pending_disputes
                .values()
                .fold(Amount::zero(), |acc, v| &acc + v);
            assert_eq!(
                a.held, pending_sum,
                "held ({}) != sum of pending_disputes ({})",
                a.held, pending_sum,
            );
        }
    }
}
