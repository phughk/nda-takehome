# Assumptions in implementation

## 1. Service cancellation
When the service is canceled, the buffer is processed and dropped.
The service should be idempotent and can be canceled at any point.
Processing the remaining buffer demonstrates that the design is capable of handling a graceful shutdown even though it does not change the implementation.

---

## Spec interpretation and behavioural assumptions

Each assumption below is verified against the implementation and covered by at least one test.

| # | Assumption | Implementation | Test |
|---|---|---|---|
| 1 | Chargeback permanently locks the account — there is no unlock operation | `src/domain/account.rs:192` | `test_locked_account_rejects_all_operations` |
| 2 | Once locked, all operations (deposit, withdrawal, dispute, resolve, chargeback) are silently ignored | `src/domain/account.rs:88–173` | `test_locked_account_rejects_all_operations` |
| 3 | A partial chargeback still locks the account immediately | `src/domain/account.rs:192` | `test_chargeback_partial_dispute` |
| 4 | Transaction lifecycle: Normal → Disputed → Resolved \| Chargeback | `src/domain/account.rs:33–47` | `test_chargeback_after_resolve` |
| 5 | A Resolved transaction can be re-disputed; disputed amounts accumulate | `src/domain/account.rs:37–39` | `test_redispute_after_resolve` |
| 6 | Chargeback is a terminal tx state — no further dispute, resolve, or chargeback on that tx ID | `src/domain/account.rs:36` | `test_chargeback_after_resolve` |
| 7 | Dispute, resolve, and chargeback on an unknown tx ID are silently ignored | `src/domain/account.rs:204–207` | `test_dispute_non_existent_tx` |
| 8 | Both deposit and withdrawal transactions can be disputed | `src/domain/account.rs:87–118` | `test_dispute_withdrawal_after` |
| 9 | Effective dispute amount = min(requested, available) | `src/domain/account.rs:126–130` | `test_dispute_exceeds_available` |
| 10 | Dispute with effective amount zero is a no-op — no funds move and no state transition occurs | `src/domain/account.rs:131–136` | `test_dispute_zero_amount` |
| 11 | Multiple disputes on the same tx accumulate the disputedAmount | `src/domain/account.rs:134` | `test_multiple_disputes_same_tx` |
| 12 | Effective resolve/chargeback amount = min(requested, disputedAmount) | `src/domain/account.rs:151–186` | `test_resolve_exceeds_disputed`, `test_chargeback_partial_dispute` |
| 13 | Partial resolve releases only the requested portion back to available; tx stays Disputed with residual disputedAmount | `src/domain/account.rs:156–166` | `test_resolve_partial` |
| 14 | Partial chargeback debits only the charged-back portion from total; releases all remaining held for that tx back to available; account still locks immediately | `src/domain/account.rs:187–194` | `test_chargeback_partial_dispute` |
| 15 | total reduced by chargeback amount only; held reduced by full disputedAmount; surplus returns to available | `src/domain/account.rs:188–191` | `test_chargeback_partial_dispute` |
| 16 | A second deposit or withdrawal reusing an existing tx ID is rejected; account is unchanged | `src/domain/account.rs:91–92, 106–107` | `test_duplicate_transaction_id`, `test_duplicate_withdrawal_id` |
| 17 | Dispute, resolve, and chargeback reference existing records and are not subject to the duplicate tx ID check | `src/domain/account.rs:120–197` | `test_multiple_disputes_same_tx` |
| 18 | Amounts are stored as a fixed-point integer with 4 decimal places of precision | `src/domain/amount.rs:6` | `src/domain/amount.rs` unit tests |
| 19 | Input amounts with more than 4 decimal places are truncated, not rounded | `src/domain/amount.rs:37–41` | `test_four_decimal_places_input` |
| 20 | Values smaller than 0.0001 are treated as zero | `src/domain/amount.rs:37–42` | `test_four_decimal_places_input` |
| 21 | Output always shows exactly 4 decimal places, zero-padded | `src/domain/amount.rs:57–71` | `test_display` |
| 22 | An empty or absent amount field is treated as zero, not an error | `src/infrastructure/csv_reader.rs:55–59` | `test_empty_amount_field_is_zero` |
| 23 | CSV whitespace is trimmed from all field values | `src/infrastructure/csv_reader.rs:37` | `src/domain/amount.rs` whitespace tests |
| 24 | An unrecognised transaction type is an error and halts processing | `src/infrastructure/csv_reader.rs:47–48` | `test_invalid_transaction_type_is_error` |
| 25 | Transactions are processed in the order they appear in the CSV | `src/infrastructure/csv_reader.rs:41–70` | `test_ordering_in_buffer` |
| 26 | Accounts are created lazily on the first transaction; initial balances are all zero and the account is unlocked | `src/service/mod.rs:95–98` | `test_handle_buffer_with_deposit` |
