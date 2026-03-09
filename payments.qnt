// -*- mode: Bluespec; -*-

/**
 * Quint specification of the toy payments engine.
 *
 * Models the account state machine described in SPEC.md and CONSTRAINTS.md:
 *
 *   - Clients identified by u16 IDs hold a single asset account.
 *   - Transactions: deposit, withdrawal, dispute, resolve, chargeback.
 *   - Every deposit/withdrawal is recorded and transitions through a
 *     lifecycle: Normal → Disputed → Resolved | Chargeback.
 *   - A chargeback permanently locks the account.
 *   - Dispute amount  = min(requested, available)        [CONSTRAINTS.md §2]
 *   - Resolve/chargeback amount = min(requested, pending)  [CONSTRAINTS.md §3]
 *   - Core invariant: total == available + held, all values ≥ 0.
 */
module payments {

    // -------------------------------------------------------------------------
    // TYPE DEFINITIONS
    // -------------------------------------------------------------------------

    // Client identifier (u16 in the implementation)
    type ClientId = int

    // Transaction identifier (u32 in the implementation)
    type TxId = int

    // Amount (plain integer; 1 unit represents 0.0001 of the real currency
    // in the implementation's 4-decimal fixed-point scheme — invariants hold
    // regardless of scale)
    type Amount = int

    // Transaction lifecycle state machine.
    //   Normal    – recorded but not disputed
    //   Disputed  – currently under dispute (funds moved to held)
    //   Resolved  – dispute resolved (funds released back to available)
    //   Chargeback – dispute resulted in reversal; account permanently locked
    type TxState = Normal | Disputed | Resolved | Chargeback

    // Per-account balance snapshot and lock flag
    type AccountState = {
        available: Amount,
        held:      Amount,
        total:     Amount,
        locked:    bool,
    }

    // Per-(client, tx) record stored by the account
    type TxRecord = {
        exists:         bool,    // true once a deposit or withdrawal recorded this tx
        amount:         Amount,  // original deposit/withdrawal amount
        state:          TxState, // current lifecycle state
        disputedAmount: Amount,  // cumulative amount currently under dispute
    }

    // -------------------------------------------------------------------------
    // CONSTANTS — parameterised per test module
    // -------------------------------------------------------------------------

    const CLIENTS: Set[ClientId]  // finite set of client IDs to consider
    const TX_IDS:  Set[TxId]      // finite set of tx IDs to consider
    const AMOUNTS: Set[Amount]    // finite set of amounts for nondeterministic steps

    // -------------------------------------------------------------------------
    // SENTINEL VALUES
    // -------------------------------------------------------------------------

    pure val NO_TX: TxRecord = {
        exists: false, amount: 0, state: Normal, disputedAmount: 0
    }

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

    // Normal, Disputed, and Resolved can all transition to Disputed
    // (cumulative / re-dispute semantics — CONSTRAINTS.md §1)
    pure def canDispute(s: TxState): bool =
        s == Normal or s == Disputed or s == Resolved

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
        val newRec  = { exists: true, amount: amount, state: Normal, disputedAmount: 0 }
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
        val newRec  = { exists: true, amount: amount, state: Normal, disputedAmount: 0 }
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
    // Preconditions: account active, tx exists, tx state ∈ {Normal, Disputed, Resolved}
    // Effective amount = min(requestedAmount, available)   [CONSTRAINTS.md §2]
    // Precondition:  effectiveAmount > 0  (silent ignore when zero)
    //
    // Effect:  available -= effectiveAmount
    //          held      += effectiveAmount
    //          total       unchanged
    //          tx.disputedAmount += effectiveAmount
    //          tx.state = Disputed
    // -------------------------------------------------------------------------
    action dispute(clientId: ClientId, txId: TxId, requestedAmount: Amount): bool = {
        val acct            = accounts.get(clientId)
        val rec             = txRecords.get((clientId, txId))
        val effectiveAmount =
            if (requestedAmount <= acct.available) requestedAmount else acct.available
        val newAcct = {
            available: acct.available - effectiveAmount,
            held:      acct.held      + effectiveAmount,
            ...acct
        }
        val newRec = {
            state:          Disputed,
            disputedAmount: rec.disputedAmount + effectiveAmount,
            ...rec
        }
        all {
            isActive(acct),
            rec.exists,
            canDispute(rec.state),
            effectiveAmount > 0,
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // RESOLVE
    //
    // Preconditions: account active, tx state == Disputed, disputedAmount > 0
    // Effective amount = min(requestedAmount, disputedAmount)  [CONSTRAINTS.md §3]
    // Precondition:  effectiveAmount > 0
    //
    // Effect:  held      -= effectiveAmount
    //          available += effectiveAmount
    //          total       unchanged
    //          tx.disputedAmount -= effectiveAmount
    //          tx.state = Resolved (if fully resolved) or Disputed (if partial)
    // -------------------------------------------------------------------------
    action resolve(clientId: ClientId, txId: TxId, requestedAmount: Amount): bool = {
        val acct            = accounts.get(clientId)
        val rec             = txRecords.get((clientId, txId))
        val effectiveAmount =
            if (requestedAmount < rec.disputedAmount) requestedAmount else rec.disputedAmount
        val remaining = rec.disputedAmount - effectiveAmount
        val newAcct = {
            held:      acct.held      - effectiveAmount,
            available: acct.available + effectiveAmount,
            ...acct
        }
        val newRec = {
            state:          if (remaining > 0) Disputed else Resolved,
            disputedAmount: remaining,
            ...rec
        }
        all {
            isActive(acct),
            rec.exists,
            canResolve(rec.state),
            rec.disputedAmount > 0,
            effectiveAmount > 0,
            accounts'  = accounts.set(clientId, newAcct),
            txRecords' = txRecords.set((clientId, txId), newRec),
        }
    }

    // -------------------------------------------------------------------------
    // CHARGEBACK
    //
    // Preconditions: account active, tx state == Disputed, disputedAmount > 0
    // Effective amount = min(requestedAmount, disputedAmount)  [CONSTRAINTS.md §3]
    // Precondition:  effectiveAmount > 0
    //
    // Effect:  held      -= disputedAmount  (clear all held for this tx)
    //          total     -= effectiveAmount
    //          available += remainingDisputed  (any non-charged-back portion released)
    //          locked     = true
    //          tx.state   = Chargeback
    //          tx.disputedAmount = 0
    // -------------------------------------------------------------------------
    action chargeback(clientId: ClientId, txId: TxId, requestedAmount: Amount): bool = {
        val acct             = accounts.get(clientId)
        val rec              = txRecords.get((clientId, txId))
        val effectiveAmount  =
            if (requestedAmount < rec.disputedAmount) requestedAmount else rec.disputedAmount
        val remainingDisputed = rec.disputedAmount - effectiveAmount
        val newAcct = {
            held:      acct.held      - rec.disputedAmount,
            total:     acct.total     - effectiveAmount,
            available: acct.available + remainingDisputed,
            locked:    true,
            ...acct
        }
        val newRec = {
            state:          Chargeback,
            disputedAmount: 0,
            ...rec
        }
        all {
            isActive(acct),
            rec.exists,
            canChargeback(rec.state),
            rec.disputedAmount > 0,
            effectiveAmount > 0,
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
            dispute(clientId, txId, amount),
            resolve(clientId, txId, amount),
            chargeback(clientId, txId, amount),
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

    // available is always non-negative
    val availableNonNegativeInv: bool =
        CLIENTS.forall(c => accounts.get(c).available >= 0)

    // held is always non-negative
    val heldNonNegativeInv: bool =
        CLIENTS.forall(c => accounts.get(c).held >= 0)

    // total is always non-negative
    val totalNonNegativeInv: bool =
        CLIENTS.forall(c => accounts.get(c).total >= 0)

    // Per-tx disputed amount is always non-negative
    val disputedAmountNonNegativeInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => txRecords.get(p).disputedAmount >= 0)

    // After a chargeback the disputed amount is cleared
    val chargebackClearsDisputeInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => {
            val rec = txRecords.get(p)
            rec.state != Chargeback or rec.disputedAmount == 0
        })

    // held can never exceed total (held is always a subset of total funds)
    val heldLeqTotalInv: bool =
        CLIENTS.forall(c => {
            val a = accounts.get(c)
            a.held <= a.total
        })

    // A tx in Resolved or Chargeback state must have zero disputedAmount
    // (can only reach those states via resolve/chargeback which clears the amount)
    val terminalStatesClearDisputeInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => {
            val rec = txRecords.get(p)
            (rec.state == Resolved or rec.state == Chargeback) implies rec.disputedAmount == 0
        })

    // A tx in Disputed state must have a positive disputedAmount
    // (dispute only transitions when effectiveAmount > 0)
    val disputedHasPositiveAmountInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => {
            val rec = txRecords.get(p)
            rec.state == Disputed implies rec.disputedAmount > 0
        })

    // A tx record that does not exist must be in the initial sentinel state
    val nonExistentTxIsCleanInv: bool =
        tuples(CLIENTS, TX_IDS).forall(p => {
            val rec = txRecords.get(p)
            not(rec.exists) implies and {
                rec.amount == 0,
                rec.state == Normal,
                rec.disputedAmount == 0,
            }
        })

    // Combined safety invariant
    val safetyInv: bool = and {
        totalEqAvailablePlusHeldInv,
        availableNonNegativeInv,
        heldNonNegativeInv,
        totalNonNegativeInv,
        heldLeqTotalInv,
        disputedAmountNonNegativeInv,
        chargebackClearsDisputeInv,
        terminalStatesClearDisputeInv,
        disputedHasPositiveAmountInv,
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

    // Use distinct names to avoid conflicts with the module constants
    pure val TEST_CLIENTS = Set(1, 2, 3)
    pure val TEST_TX_IDS  = Set(1, 2, 3, 4, 5)
    pure val TEST_AMOUNTS = Set(100, 200, 300, 500, 800, 1000)

    import payments(
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

    // Full dispute then full resolve restores available; account stays unlocked
    run disputeResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(all {
                assert(accounts.get(1).available == 500),
                assert(accounts.get(1).held       == 500),
                assert(accounts.get(1).total      == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
            .then(resolve(1, 1, 500))
            .then(all {
                assert(accounts.get(1).available == 1000),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == 1000),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Partial resolve: dispute 500, resolve 300 → held = 200, available = 800
    run partialResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(resolve(1, 1, 300))
            .then(all {
                assert(accounts.get(1).available == 800),
                assert(accounts.get(1).held       == 200),
                assert(accounts.get(1).total      == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Resolve on a non-disputed tx must fail
    run resolveWithoutDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(resolve(1, 1, 500).fail())
    }

    // -------------------------------------------------------------------------
    // DISPUTE → CHARGEBACK
    // -------------------------------------------------------------------------

    // Full dispute then full chargeback: account locked, total halved
    run disputeChargebackTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 500))
            .then(all {
                assert(accounts.get(1).available == 500),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == 500),
                assert(accounts.get(1).locked),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Partial chargeback: dispute 500, chargeback 300
    // held clears all 500; total -= 300; remaining 200 released to available
    run partialChargebackTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 300))
            .then(all {
                assert(accounts.get(1).available == 700),
                assert(accounts.get(1).held       == 0),
                assert(accounts.get(1).total      == 700),
                assert(accounts.get(1).locked),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Chargeback without a prior dispute must fail
    run chargebackWithoutDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(chargeback(1, 1, 500).fail())
    }

    // Chargeback after a full resolve must fail (tx is Resolved, not Disputed)
    run chargebackAfterResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(resolve(1, 1, 500))
            .then(chargeback(1, 1, 500).fail())
    }

    // -------------------------------------------------------------------------
    // LOCKED ACCOUNT REJECTS ALL OPERATIONS
    // -------------------------------------------------------------------------

    run lockedAccountRejectsDepositTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 500))
            .then(deposit(1, 2, 100).fail())
    }

    run lockedAccountRejectsWithdrawalTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 500))
            .then(withdrawal(1, 2, 100).fail())
    }

    run lockedAccountRejectsDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(deposit(1, 2, 500))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 500))
            .then(dispute(1, 2, 200).fail())
    }

    // -------------------------------------------------------------------------
    // EDGE CASES
    // -------------------------------------------------------------------------

    // Dispute on a tx that was never deposited must fail
    run disputeNonExistentTxTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 2, 500).fail())    // tx=2 was never recorded
    }

    // Dispute amount exceeding available is capped at available.
    // Deposit 1000, withdraw 800 → available = 200. Dispute 500 → only 200 held.
    run disputeExceedsAvailableTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 800))
            .then(dispute(1, 1, 500))
            .then(all {
                assert(accounts.get(1).available == 0),
                assert(accounts.get(1).held       == 200),
                assert(accounts.get(1).total      == 200),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Multiple cumulative disputes on the same tx (CONSTRAINTS.md §1)
    run multipleCumulativeDisputesTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 300))
            .then(dispute(1, 1, 200))
            .then(all {
                assert(accounts.get(1).available == 500),
                assert(accounts.get(1).held       == 500),
                assert(accounts.get(1).total      == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Re-dispute after resolve is allowed (CONSTRAINTS.md §1)
    run redisputeAfterResolveTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))
            .then(resolve(1, 1, 500))
            .then(dispute(1, 1, 300))
            .then(all {
                assert(accounts.get(1).available == 700),
                assert(accounts.get(1).held       == 300),
                assert(accounts.get(1).total      == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // CLIENT ISOLATION
    // -------------------------------------------------------------------------

    // Chargeback on client 1 must not affect client 2
    run clientIsolationTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(deposit(2, 2, 1000))
            .then(dispute(1, 1, 500))
            .then(chargeback(1, 1, 500))
            .then(all {
                assert(accounts.get(1).locked),
                assert(accounts.get(1).total     == 500),
                assert(not(accounts.get(2).locked)),
                assert(accounts.get(2).available == 1000),
                assert(accounts.get(2).total     == 1000),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Two clients can dispute their own transactions independently
    run independentDisputesTest = {
        init
            .then(deposit(1, 1, 500))
            .then(deposit(2, 2, 500))
            .then(dispute(1, 1, 300))
            .then(dispute(2, 2, 200))
            .then(all {
                assert(accounts.get(1).available == 200),
                assert(accounts.get(1).held       == 300),
                assert(accounts.get(2).available == 300),
                assert(accounts.get(2).held       == 200),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // WITHDRAWAL AFTER DISPUTE  [CONSTRAINTS.md §8]
    //
    // "Withdrawal only affects available funds. Held funds from dispute
    //  are protected."
    // -------------------------------------------------------------------------

    // Dispute moves funds to held; a subsequent withdrawal can only draw from
    // the remaining available — held is untouched.
    run withdrawalAfterDisputeTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 500))         // available = 500, held = 500
            .then(withdrawal(1, 2, 300))      // available -= 300 → 200
            .then(all {
                assert(accounts.get(1).available == 200),
                assert(accounts.get(1).held       == 500),
                assert(accounts.get(1).total      == 700),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // Withdrawal cannot draw from held funds — if available < amount, it fails
    // even though held would cover the deficit.
    run withdrawalCannotDrawFromHeldTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, 800))         // available = 200, held = 800
            .then(withdrawal(1, 2, 500).fail()) // 500 > available(200) → rejected
    }

    // -------------------------------------------------------------------------
    // DISPUTE ON A WITHDRAWAL TX
    //
    // The spec says disputes reference any prior transaction by ID.
    // Withdrawals are recorded just like deposits, so they can be disputed.
    // -------------------------------------------------------------------------

    run disputeWithdrawalTxTest = {
        init
            .then(deposit(1, 1, 1000))
            .then(withdrawal(1, 2, 300))      // available = 700
            .then(dispute(1, 2, 200))         // dispute the withdrawal tx
            .then(all {
                assert(accounts.get(1).available == 500),
                assert(accounts.get(1).held       == 200),
                assert(accounts.get(1).total      == 700),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // -------------------------------------------------------------------------
    // RANDOMIZED PROPERTY TESTS
    //
    // These use nondet to pick arbitrary valid parameters and check that
    // postconditions hold after every successful operation.
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

    // After a deposit of 1000 and a successful dispute with a nondet amount,
    // total is unchanged and available + held == 1000.
    run disputePostconditionTest = {
        nondet amount = oneOf(TEST_AMOUNTS)
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, amount))
            .then(all {
                // total unchanged; whatever was moved to held came from available
                assert(accounts.get(1).total == 1000),
                assert(accounts.get(1).available + accounts.get(1).held == 1000),
                assert(accounts.get(1).held >= 0),
                assert(not(accounts.get(1).locked)),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }

    // After a deposit, any successful dispute, and a matching chargeback,
    // the account is always locked.
    run chargebackAlwaysLocksTest = {
        nondet amount = oneOf(TEST_AMOUNTS)
        init
            .then(deposit(1, 1, 1000))
            .then(dispute(1, 1, amount))
            .then(chargeback(1, 1, amount))
            .then(all {
                assert(accounts.get(1).locked),
                assert(accounts.get(1).held == 0),
                accounts'  = accounts,
                txRecords' = txRecords,
            })
    }
}
