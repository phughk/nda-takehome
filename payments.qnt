// -*- mode: Bluespec; -*-

/**
 * Quint specification of the toy payments engine.
 *
 * Models the account state machine in src/domain/account.rs:
 *
 *   - Clients identified by u16 IDs hold a single asset account.
 *   - Transactions: deposit, withdrawal, dispute, resolve, chargeback.
 *   - Every deposit/withdrawal is recorded and transitions through a
 *     lifecycle: Normal → Disputed → Resolved | Chargeback.
 *   - Dispute/resolve/chargeback always use the *stored original transaction
 *     amount* (rec.amount); any amount field in those messages is ignored.
 *   - Resolved is a terminal state — no re-dispute or chargeback after resolve.
 *   - A transaction can only be disputed once; a second dispute on a Disputed
 *     tx is rejected (AlreadyDisputed).
 *   - A chargeback permanently locks the account.
 *   - Core invariant: total == available + held.
 *   - available may go negative when a deposit is disputed after a partial
 *     withdrawal (the full original deposit amount is always moved to held).
 *   - total may go negative on a locked account for the same reason.
 */
module paymentsSpec {

    // -------------------------------------------------------------------------
    // TYPE DEFINITIONS
    // -------------------------------------------------------------------------

    // Client identifier (u16 in the implementation)
    type ClientId = int

    // Transaction identifier (u32 in the implementation)
    type TxId = int

    // Amount (plain integer; 1 unit represents 0.0001 of the real currency
    // in the implementation's 4-decimal fixed-point scheme)
    type Amount = int

    // Transaction lifecycle state machine.
    //   Normal     – recorded but not disputed
    //   Disputed   – currently under dispute (funds moved to held)
    //   Resolved   – dispute resolved; terminal — cannot be re-disputed
    //   Chargeback – dispute resulted in reversal; account permanently locked; terminal
    type TxState = Normal | Disputed | Resolved | Chargeback

    // Per-account balance snapshot and lock flag
    type AccountState = {
        available: Amount,
        held:      Amount,
        total:     Amount,
        locked:    bool,
    }

    // Per-(client, tx) record stored by the account.
    // `amount` holds the original deposit/withdrawal amount and is what
    // dispute/resolve/chargeback operate on — not any amount in those messages.
    type TxRecord = {
        exists: bool,    // true once a deposit or withdrawal recorded this tx
        amount: Amount,  // original deposit/withdrawal amount
        state:  TxState, // current lifecycle state
    }

    // -------------------------------------------------------------------------
    // CONSTANTS — parameterised per test module
    // -------------------------------------------------------------------------

    const CLIENTS: Set[ClientId]
    const TX_IDS:  Set[TxId]
    const AMOUNTS: Set[Amount]

    // -------------------------------------------------------------------------
    // SENTINEL VALUES
    // -------------------------------------------------------------------------

    pure val NO_TX: TxRecord = { exists: false, amount: 0, state: Normal }

    pure val EMPTY_ACCOUNT: AccountState = {
        available: 0, held: 0, total: 0, locked: false
    }

    // -------------------------------------------------------------------------
    // STATE VARIABLES
    // -------------------------------------------------------------------------

    var accounts:  ClientId -> AccountState        // balance & lock state per client
    var txRecords: (ClientId, TxId) -> TxRecord    // lifecycle record per (client, tx)

    // -------------------------------------------------------------------------
    // PURE HELPERS
    // -------------------------------------------------------------------------

    pure def isActive(acct: AccountState): bool = not(acct.locked)

    // Only Normal transactions can be disputed.
    // Disputed rejects a second dispute (AlreadyDisputed).
    // Resolved is a terminal state — no further transitions.
    pure def canDispute(s: TxState): bool = s == Normal

    pure def canResolve(s: TxState): bool = s == Disputed

    pure def canChargeback(s: TxState): bool = s == Disputed

    // -------------------------------------------------------------------------
    // INITIALIZATION
    // -------------------------------------------------------------------------

    action init: bool = all {
        accounts'  = CLIENTS.mapBy(_ => EMPTY_ACCOUNT),
        txRecords' = tuples(CLIENTS, TX_IDS).mapBy(_ => NO_TX),
    }

    // -------------------------------------------------------------------------
    // DEPOSIT
    //
    // Preconditions: account active, tx ID unused, amount > 0
    // Effect:  available += amount,  total += amount
    // -------------------------------------------------------------------------
    action deposit(clientId: ClientId, txId: TxId, amount: Amount): bool = {
        val acct    = accounts.get(clientId)
        val rec     = txRecords.get((clientId, txId))
        val newAcct = { available: acct.available + amount, total: acct.total + amount, ...acct }
        val newRec  = { exists: true, amount: amount, state: Normal }
        all {
            amount > 0,
            isActive(acct),
            not(rec.exists),
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // WITHDRAWAL
    //
    // Preconditions: account active, tx ID unused, amount > 0, available >= amount
    // Effect:  available -= amount,  total -= amount
    // -------------------------------------------------------------------------
    action withdrawal(clientId: ClientId, txId: TxId, amount: Amount): bool = {
        val acct    = accounts.get(clientId)
        val rec     = txRecords.get((clientId, txId))
        val newAcct = { available: acct.available - amount, total: acct.total - amount, ...acct }
        val newRec  = { exists: true, amount: amount, state: Normal }
        all {
            amount > 0,
            isActive(acct),
            not(rec.exists),
            acct.available >= amount,
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // DISPUTE
    //
    // Preconditions: account active, tx exists, tx state == Normal
    // Amount: the stored original transaction amount (rec.amount) is used;
    //         any amount field in the dispute message is ignored.
    //
    // Effect:  available -= rec.amount  (may go negative if a withdrawal occurred
    //                                    before the dispute)
    //          held      += rec.amount
    //          total       unchanged
    //          tx.state    = Disputed
    // -------------------------------------------------------------------------
    action dispute(clientId: ClientId, txId: TxId): bool = {
        val acct   = accounts.get(clientId)
        val rec    = txRecords.get((clientId, txId))
        val amt    = rec.amount
        val newAcct = {
            available: acct.available - amt,
            held:      acct.held      + amt,
            ...acct
        }
        val newRec = { state: Disputed, ...rec }
        all {
            isActive(acct),
            rec.exists,
            canDispute(rec.state),
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // RESOLVE
    //
    // Preconditions: account active, tx state == Disputed
    // Amount: the stored original transaction amount (rec.amount) is used;
    //         any amount field in the resolve message is ignored.
    //
    // Effect:  held      -= rec.amount
    //          available += rec.amount
    //          total       unchanged
    //          tx.state    = Resolved  (terminal — cannot be re-disputed or charged back)
    // -------------------------------------------------------------------------
    action resolve(clientId: ClientId, txId: TxId): bool = {
        val acct   = accounts.get(clientId)
        val rec    = txRecords.get((clientId, txId))
        val amt    = rec.amount
        val newAcct = {
            held:      acct.held      - amt,
            available: acct.available + amt,
            ...acct
        }
        val newRec = { state: Resolved, ...rec }
        all {
            isActive(acct),
            rec.exists,
            canResolve(rec.state),
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // CHARGEBACK
    //
    // Preconditions: account active, tx state == Disputed
    // Amount: the stored original transaction amount (rec.amount) is used;
    //         any amount field in the chargeback message is ignored.
    //
    // Effect:  held   -= rec.amount
    //          total  -= rec.amount  (may go negative on a locked account if
    //                                 the deposit was partially withdrawn before
    //                                 being disputed)
    //          locked  = true
    //          tx.state = Chargeback  (terminal)
    // -------------------------------------------------------------------------
    action chargeback(clientId: ClientId, txId: TxId): bool = {
        val acct   = accounts.get(clientId)
        val rec    = txRecords.get((clientId, txId))
        val amt    = rec.amount
        val newAcct = {
            held:   acct.held  - amt,
            total:  acct.total - amt,
            locked: true,
            ...acct
        }
        val newRec = { state: Chargeback, ...rec }
        all {
            isActive(acct),
            rec.exists,
            canChargeback(rec.state),
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // STEP — all possible behaviours in one action (for model checking / fuzzing)
    // -------------------------------------------------------------------------
    action step: bool = {
        nondet clientId = oneOf(CLIENTS)
        nondet txId     = oneOf(TX_IDS)
        nondet amount   = oneOf(AMOUNTS)
        any {
            deposit(clientId, txId, amount),
            withdrawal(clientId, txId, amount),
            dispute(clientId, txId),
            resolve(clientId, txId),
            chargeback(clientId, txId),
        }
    }

    // -------------------------------------------------------------------------
    // INVARIANTS
    // -------------------------------------------------------------------------

    // Core balance invariant: total == available + held for every account
    val totalEqAvailablePlusHeldInv: bool =
        CLIENTS.forall(c => {
            val a = accounts.get(c)
            a.total == a.available + a.held
        })

    // held is always non-negative
    val heldNonNegativeInv: bool =
        CLIENTS.forall(c => accounts.get(c).held >= 0)

    // total is non-negative for unlocked accounts.
    // A locked (post-chargeback) account may have a negative total when the
    // chargedback deposit had already been partially withdrawn before the dispute.
    val totalNonNegativeUnlessLockedInv: bool =
        CLIENTS.forall(c => {
            val a = accounts.get(c)
            a.locked or a.total >= 0
        })

    // A locked account must have at least one chargeback recorded
    val lockedImpliesChargebackInv: bool =
        CLIENTS.forall(c => {
            val a = accounts.get(c)
            not(a.locked) or tuples(Set(c), TX_IDS).exists(p => txRecords.get(p).state == Chargeback)
        })

    // A tx record that does not exist must be in the initial sentinel state
    val nonExistentTxIsCleanInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => {
            val rec = txRecords.get(p)
            not(rec.exists) implies and {
                rec.amount == 0,
                rec.state  == Normal,
            }
        })

    // Combined safety invariant
    val safetyInv: bool = and {
        totalEqAvailablePlusHeldInv,
        heldNonNegativeInv,
        totalNonNegativeUnlessLockedInv,
        lockedImpliesChargebackInv,
        nonExistentTxIsCleanInv,
    }

    // -------------------------------------------------------------------------
    // TEMPORAL PROPERTIES
    // -------------------------------------------------------------------------

    // Once an account is locked it remains locked forever
    temporal lockedAccountsStayLockedProp: bool =
        CLIENTS.forall(c =>
            always(accounts.get(c).locked implies always(accounts.get(c).locked))
        )
}

// =============================================================================
// TEST MODULE
//
// Run all invariants:
//   quint run --invariant=safetyInv --main=paymentsTests payments.qnt
//
// Run a specific test:
//   quint run --main=paymentsTests --run=disputeChargebackTest payments.qnt
// =============================================================================
module paymentsTests {

    pure val TEST_CLIENTS = Set(1, 2, 3)
    pure val TEST_TX_IDS  = Set(1, 2, 3, 4, 5)
    pure val TEST_AMOUNTS = Set(100, 200, 300, 500, 800, 1000)

    import paymentsSpec(
        CLIENTS = TEST_CLIENTS,
        TX_IDS  = TEST_TX_IDS,
        AMOUNTS = TEST_AMOUNTS
    ).*

    // -------------------------------------------------------------------------
    // BASIC DEPOSIT / WITHDRAWAL
    // -------------------------------------------------------------------------

    // Deposit 1000 then withdraw 300 → available = 700
    run depositWithdrawalTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 300))
            .then(all {
                assert(accounts.get(1).available == 700),
                assert(accounts.get(1).total     == 700),
                assert(accounts.get(1).held       == 0),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Withdrawal before any deposit must fail (no funds)
    run withdrawalWithoutFundsTest = {
        init.then(withdrawal(1, 1, 500).fail())
    }

    // Withdrawal exceeding available balance must fail
    run insufficientFundsWithdrawalTest = {
        init
            .then(deposit(1, 1, 100))
            .then(withdrawal(1, 2, 500).fail())
    }

    // Duplicate tx ID must be rejected
    run duplicateTxIdTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(deposit(1, 1, 500).fail())
    }

    // -------------------------------------------------------------------------
    // DISPUTE → RESOLVE
    // -------------------------------------------------------------------------

    // Dispute then resolve: the full stored deposit amount (1000) moves to held
    // then back to available. Any amount in the dispute/resolve message is ignored.
    run disputeResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(all {
                assert(accounts.get(1).available == 0),
                assert(accounts.get(1).held       == 1000),
                assert(accounts.get(1).total      == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
            .then(resolve(1, 1))
            .then(all {
                assert(accounts.get(1).available == 1000),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == 1000),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Resolve on a non-disputed tx must fail
    run resolveWithoutDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(resolve(1, 1).fail())
    }

    // -------------------------------------------------------------------------
    // DISPUTE → CHARGEBACK
    // -------------------------------------------------------------------------

    // Full dispute then chargeback: entire deposit removed, account locked.
    // available = 0, held = 0, total = 0.
    run disputeChargebackTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(all {
                assert(accounts.get(1).available == 0),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == 0),
                assert(accounts.get(1).locked),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Chargeback without a prior dispute must fail
    run chargebackWithoutDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(chargeback(1, 1).fail())
    }

    // Chargeback after a full resolve must fail (Resolved is a terminal state)
    run chargebackAfterResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(resolve(1, 1))
            .then(chargeback(1, 1).fail())
    }

    // -------------------------------------------------------------------------
    // LOCKED ACCOUNT REJECTS ALL OPERATIONS
    // -------------------------------------------------------------------------

    run lockedAccountRejectsDepositTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(deposit(1, 2, 100).fail())
    }

    run lockedAccountRejectsWithdrawalTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(withdrawal(1, 2, 100).fail())
    }

    run lockedAccountRejectsDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(deposit(1, 2, 500))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(dispute(1, 2).fail())
    }

    // -------------------------------------------------------------------------
    // EDGE CASES
    // -------------------------------------------------------------------------

    // Dispute on a tx that was never deposited must fail
    run disputeNonExistentTxTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 2).fail())    // tx=2 was never recorded
    }

    // Dispute uses the stored original deposit amount, not min(requested, available).
    // Deposit 1000, withdraw 800 → available = 200.
    // Dispute tx=1: uses stored 1000 → available = 200 - 1000 = -800, held = 1000, total = 200.
    run disputeExceedsAvailableTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 800))
            .then(dispute(1, 1))
            .then(all {
                assert(accounts.get(1).available == -800),
                assert(accounts.get(1).held       == 1000),
                assert(accounts.get(1).total      == 200),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // A second dispute on the same (already Disputed) transaction must fail
    run doubleDisputeRejectedTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(dispute(1, 1).fail())
    }

    // Re-dispute after resolve must fail (Resolved is a terminal state)
    run redisputeAfterResolveFailsTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(resolve(1, 1))
            .then(dispute(1, 1).fail())
    }

    // -------------------------------------------------------------------------
    // CLIENT ISOLATION
    // -------------------------------------------------------------------------

    // Chargeback on client 1 must not affect client 2
    run clientIsolationTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(deposit(2, 2, 1000))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(all {
                assert(accounts.get(1).locked),
                assert(accounts.get(1).available == 0),
                assert(accounts.get(1).total     == 0),
                assert(not(accounts.get(2).locked)),
                assert(accounts.get(2).available == 1000),
                assert(accounts.get(2).total     == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Two clients can dispute their own transactions independently;
    // each dispute moves the full stored amount to held.
    run independentDisputesTest = {
        init
            .then(deposit(1, 1, 500))
            .then(deposit(2, 2, 500))
            .then(dispute(1, 1))
            .then(dispute(2, 2))
            .then(all {
                assert(accounts.get(1).available == 0),
                assert(accounts.get(1).held       == 500),
                assert(accounts.get(2).available == 0),
                assert(accounts.get(2).held       == 500),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // WITHDRAWAL AFTER DISPUTE  [SPEC.md §withdrawal]
    //
    // After a dispute the full deposit amount moves to held, leaving available = 0.
    // Subsequent withdrawals fail because nothing remains in available.
    // -------------------------------------------------------------------------

    // Dispute moves the full 1000 to held; withdrawal of 1 fails (available = 0)
    run withdrawalAfterDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))              // available = 0, held = 1000
            .then(withdrawal(1, 2, 1).fail()) // available = 0 → InsufficientBalance
    }

    // Withdrawal cannot draw from held funds even when held would cover it
    run withdrawalCannotDrawFromHeldTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))                // available = 0, held = 1000
            .then(withdrawal(1, 2, 500).fail()) // 500 > available(0) → rejected
    }

    // -------------------------------------------------------------------------
    // DISPUTE ON A WITHDRAWAL TX
    //
    // Withdrawals are recorded just like deposits; they can be disputed.
    // The dispute uses the stored withdrawal amount (300).
    // -------------------------------------------------------------------------

    run disputeWithdrawalTxTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 300))      // available = 700
            .then(dispute(1, 2))              // uses stored 300: available = 400, held = 300
            .then(all {
                assert(accounts.get(1).available == 400),
                assert(accounts.get(1).held       == 300),
                assert(accounts.get(1).total      == 700),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // CHARGEBACK ON A PARTIALLY WITHDRAWN DEPOSIT
    //
    // If a deposit is disputed after a partial withdrawal, the chargeback removes
    // the full original deposit amount from total, potentially leaving a negative
    // total on the locked account.
    // -------------------------------------------------------------------------

    // Deposit 1000, withdraw 1, dispute tx=1, chargeback tx=1:
    //   after withdraw:   available = 999, total = 999
    //   after dispute:    available = -1,  held = 1000, total = 999
    //   after chargeback: held = 0, total = -1, locked = true
    run chargebackNegativeTotalTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 1))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(all {
                assert(accounts.get(1).available == -1),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == -1),
                assert(accounts.get(1).locked),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // RANDOMIZED PROPERTY TESTS
    // -------------------------------------------------------------------------

    // After a fresh deposit with a nondet amount, available == amount,
    // total == amount, held == 0, account unlocked.
    run depositPostconditionTest = {
        nondet amount = oneOf(TEST_AMOUNTS)
        init
            .then(deposit(1, 1, amount))
            .then(all {
                assert(accounts.get(1).available == amount),
                assert(accounts.get(1).total     == amount),
                assert(accounts.get(1).held      == 0),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // After deposit(1000) then dispute, the full 1000 is in held, available = 0,
    // total is unchanged.
    run disputePostconditionTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(all {
                assert(accounts.get(1).total     == 1000),
                assert(accounts.get(1).held      == 1000),
                assert(accounts.get(1).available == 0),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // After deposit, dispute, and chargeback the account is always locked
    // and both held and total are reduced to zero (when no prior withdrawal).
    run chargebackAlwaysLocksTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1))
            .then(chargeback(1, 1))
            .then(all {
                assert(accounts.get(1).locked),
                assert(accounts.get(1).held  == 0),
                assert(accounts.get(1).total == 0),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }
}

// =============================================================================
// DEFAULT ENTRY-POINT
//
// This module matches the filename (`payments.qnt`) so `quint run` and
// `quint verify` use it automatically without requiring `--main`.
//
// Run random simulation:
//   quint run payments.qnt
//
// Run invariant verification:
//   quint verify payments.qnt --invariant=safetyInv
//
// Run unit tests:
//   quint test --main=paymentsTests payments.qnt
// =============================================================================
module payments {

    import paymentsSpec(
        CLIENTS = Set(1, 2, 3),
        TX_IDS  = Set(1, 2, 3, 4),
        AMOUNTS = Set(100, 500, 1000)
    ).*
}
