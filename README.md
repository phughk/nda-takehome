# Payments Engine

A toy payments engine that reads a CSV of transactions, processes deposits, withdrawals, disputes, resolutions, and
chargebacks, and writes the final account state to stdout.

## Assumptions

### Locked accounts

- A chargeback **permanently locks** the account. There is no unlock operation.
- Once locked, **all** operations (deposit, withdrawal, dispute, resolve, chargeback) are silently ignored — the account
  remains frozen with its balance intact.
- A partial chargeback still locks the account immediately. The lock is unconditional on the chargeback action itself,
  not on the amount.

### Transaction lifecycle

- The lifecycle for a deposit or withdrawal is: `Normal → Disputed → Resolved | Chargeback`.
- A `Resolved` transaction **can be re-disputed**. Subsequent disputes accumulate on top of the existing
  `disputedAmount` (CONSTRAINTS.md §1: "disputed amounts are cumulative"). This allows a fraudster's reversal to be
  caught after an initial failed dispute.
- A `Chargeback` is a terminal state. No further dispute, resolve, or chargeback is permitted on that transaction ID.
- Dispute, resolve, and chargeback are only valid on transactions that were originally recorded as a deposit or
  withdrawal. References to unknown transaction IDs are silently ignored.
- Both **deposit and withdrawal** transactions can be disputed. The spec does not restrict disputes to deposits only.

### Dispute semantics

- The effective dispute amount is `min(requested, available)`. If available funds are insufficient to cover the full
  requested amount, the dispute is partially applied up to what is available.
- A dispute with an effective amount of zero (e.g. requested > 0 but `available == 0`) is a no-op — no state transition
  occurs and no funds move.
- Multiple disputes on the same transaction accumulate: each successive dispute moves additional funds from `available`
  to `held`.

### Resolve and chargeback semantics

- The effective resolve/chargeback amount is `min(requested, disputedAmount)`. Requests exceeding the outstanding
  disputed amount are capped silently.
- A **partial resolve** releases only the requested portion back to `available`; the transaction remains in `Disputed`
  state with the residual `disputedAmount`.
- A **partial chargeback** debits only the charged-back portion from `total`, but releases **all** remaining held funds
  for that transaction back to `available` (the non-charged-back disputed portion is no longer held). The account is
  still locked immediately.
- `total` is only reduced by the chargeback amount, not by the full disputed amount. `held` is reduced by the full
  disputed amount. Any surplus returns to `available`.

### Duplicate transaction IDs

- Transaction IDs are unique per client. A second deposit or withdrawal reusing an existing ID is rejected and the
  account is unchanged.
- Dispute, resolve, and chargeback reference existing IDs and do not create new records, so they are not subject to the
  duplicate check.

### Amount precision

- All amounts are stored as a fixed-point integer with 4 decimal places of precision (`1 unit = 0.0001`). Arithmetic is
  exact with no floating-point rounding.
- Input amounts with more than 4 decimal places are **truncated** (not rounded): `1.00009` → `1.0000`.
- Values smaller than `0.0001` (e.g. `0.00009`) are treated as zero.
- Output always shows exactly 4 decimal places, zero-padded: `5` → `5.0000`.

### Input handling

- The `amount` field is optional for dispute, resolve, and chargeback rows. An empty or absent amount is treated as
  zero.
- CSV whitespace (spaces around field values) is trimmed.
- An unrecognised transaction type is an error and halts processing.
- Transactions are processed in the order they appear in the CSV (chronological order). Transaction IDs are not assumed
  to be ordered.

### Account creation

- Accounts are created lazily on first transaction. A client referenced for the first time starts with `available = 0`,
  `held = 0`, `total = 0`, `locked = false`.

---

## Running the engine

```bash
cargo run -- transactions.csv > accounts.csv
```

The input file path is the only argument. Output is written to stdout in CSV format.

### Output format

```
client,available,held,total,locked
1,75.5000,0.0000,75.5000,false
2,200.0000,0.0000,200.0000,false
```

All monetary values are printed with exactly four decimal places of precision.

## Running the tests

```bash
# Unit and integration tests
cargo test

# With output visible
cargo test -- --nocapture

# Run a specific test
cargo test chargeback
```

### Property tests

The `tests/property_tests.rs` file contains property-based tests powered
by [proptest](https://crates.io/crates/proptest). These generate random CSV transaction sequences and verify that
system-wide invariants always hold.

```bash
# Run all property tests (200 random cases each)
cargo test --test property_tests

# Run a single property test
cargo test --test property_tests total_equals_available_plus_held
```

The tests cover two categories:

**Fuzz invariants** — random mixes of deposits, withdrawals, disputes, resolves, and chargebacks verified against:

- Engine never panics on any valid input
- `total == available + held`
- `available`, `held`, and `total` are never negative

**Targeted properties** — structured random inputs verifying:

- Deposit-only sums are correct
- Full withdrawal zeros the balance
- Dispute/resolve round-trip is lossless
- Chargeback subtracts the disputed amount and locks the account
- Locked accounts reject all further mutations
- Withdrawals cannot overdraw
- Duplicate transaction IDs are rejected
- Disputing a non-existent transaction is a no-op
- Client accounts are fully isolated
- 4-decimal-place amounts survive CSV round-trip exactly

### Lifecycle property tests

The `tests/lifecycle_property_tests.rs` file contains property-based tests that drive the `Account` state machine
directly (no CSV round-trip). This allows inspection of internal state — transaction state sets, pending disputes, held
amounts, and the locked flag — after every single operation in a randomly generated sequence.

```bash
# Run all lifecycle property tests (300 random cases each)
cargo test --test lifecycle_property_tests

# Run a single lifecycle test
cargo test --test lifecycle_property_tests chargeback_locks_permanently
```

The tests verify:

**Full state-machine fuzzing** — random deposit/withdrawal/dispute/resolve/chargeback sequences with invariant checks
after every step:

- `total == available + held` at every step
- `available`, `held`, `total` never negative at any step
- Each transaction appears in exactly one state set (normal, disputed, resolved, or chargeback)
- Pending dispute entries only exist for transactions in the disputed state

**Transaction lifecycle properties:**

- Deposit/withdrawal sequences never produce negative balances
- Chargeback permanently locks the account — all subsequent operations return `AccountLocked` and leave balances frozen
- Dispute/resolve is a lossless round-trip (available and held return to pre-dispute values)
- Multiple disputes on the same transaction accumulate correctly and a full resolve releases everything
- Chargeback reduces total by exactly the chargeback amount and zeroes held
- Duplicate transaction IDs always return `DuplicateTransaction`
- Overdraw attempts always return `InsufficientBalance`
- Disputes on non-existent transactions return `InvalidTransaction` with no side effects
- Resolves on non-disputed transactions return `TransactionNotDisputed`
- Resolved transactions can be re-disputed, moving funds back into held
- Multi-transaction accounts with mixed deposits, withdrawals, and lifecycle actions maintain all invariants

### Fuzz tests (single account)

The `tests/fuzz_single_account.rs` file throws completely random, unstructured operation sequences at a single `Account`
and triggers the exhaustive `AccountInvariantGuard` after every step. Unlike the lifecycle tests above, these do not
pre-seed deposits or follow any realistic pattern — the goal is to exercise every code path with arbitrary input and
verify nothing crashes or corrupts internal state.

```bash
# Run all fuzz tests (500 random cases each, up to 100 ops per case)
cargo test --test fuzz_single_account

# Run a single fuzz test
cargo test --test fuzz_single_account single_tx_id_hammered
```

The `AccountInvariantGuard` (in `src/domain/account.rs`) checks the following after every operation:

- **Balances**: `available >= 0`, `held >= 0`, `total >= 0`, `total == available + held`
- **Set membership**: every transaction in the map appears in exactly one state set (normal, disputes, resolves,
  chargebacks) and vice versa — no orphans in either direction
- **State consistency**: the set each transaction is in matches its recorded `TransactionState` enum value
- **Set disjointness**: all six pairwise intersections between the four state sets are empty
- **Set-map size**: the sum of the four set sizes equals the transactions map size
- **Pending disputes**: every `pending_disputes` key is in the disputes set with a positive amount; every disputes-set
  member has a `pending_disputes` entry
- **Held consistency**: `held` equals the sum of all `pending_disputes` values
- **Lock semantics**: if locked, at least one chargeback exists

The five test variants cover:

- **random_ops_never_violate_invariants** — uniform random ops, small tx ID space (1–10)
- **random_ops_wide_tx_ids** — uniform random ops, wide tx ID space (1–1000)
- **deposit_heavy_then_random_lifecycle** — deposits first, then random lifecycle actions
- **zero_amount_ops_are_safe** — all amounts are zero
- **single_tx_id_hammered** — one seeded deposit, then all ops target the same tx ID

## Quint formal specification

The file `payments.qnt` contains a formal specification of the payments engine written
in [Quint](https://github.com/informalsystems/quint). It models the account state machine and verifies safety invariants
via random simulation.

I used this partially during validating understanding and the codebase. It can be helpful.
There is also a Invariant checker inside the account implementation that is run in debug builds - it checks properties
at runtime and is dropped in release builds.

### Running Quint with Nix

Start a shell with Quint available (no installation required):

```bash
nix shell "github:NixOS/nixpkgs#quint" \
    --extra-experimental-features "nix-command flakes"
```

Then run the tests or simulator directly:

```bash
# Run all named tests
quint test --main=paymentsTests payments.qnt

# Random simulation — check the combined safety invariant
quint run --main=paymentsTests --invariant=safetyInv payments.qnt

# Check a specific invariant with more samples
quint run --main=paymentsTests \
    --invariant=totalEqAvailablePlusHeldInv \
    --max-samples=50000 \
    payments.qnt
```

## Docker image

The `Dockerfile.quint` image bundles the spec and a pre-populated Nix store so no network access is needed at runtime.

### Build

```bash
docker build -f Dockerfile.quint -t quint-payments .
```

### Usage

```bash
# Run all unit tests (default)
docker run --rm quint-payments

# Equivalent explicit form
docker run --rm quint-payments test

# Filter tests by name pattern
docker run --rm quint-payments test --match=chargeback

# Check the combined safety invariant (~10 000 random traces)
docker run --rm quint-payments check

# Check a specific invariant
docker run --rm quint-payments check totalEqAvailablePlusHeldInv

# Tune the simulator
docker run --rm quint-payments check safetyInv --max-samples=50000

# Check every invariant in sequence
docker run --rm quint-payments check-all

# Pass arbitrary flags to quint run (--main is pre-set)
docker run --rm quint-payments run --max-samples=5000 --max-steps=30

# Raw quint invocation (no implicit spec path)
docker run --rm quint-payments -- quint parse /spec/payments.qnt
```
