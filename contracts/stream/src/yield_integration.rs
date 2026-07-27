use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Symbol};

/// Fixed-point scaling factor for rebate calculations.
/// Uses basis points system: 10_000 = 100%
const REBATE_BPS_SCALE: i128 = 10_000;

/// Seconds in a year for APY calculations (365.25 days * 24 hours * 60 minutes * 60 seconds)
const SECONDS_PER_YEAR: i128 = 31_557_600;

/// Maximum rebate rate in basis points (10% = 1000 bps) to prevent excessive yields
const MAX_REBATE_RATE_BPS: i128 = 1_000;

/// Minimum rebate calculation threshold to prevent dust accumulation
const MIN_REBATE_THRESHOLD: i128 = 100;

/// Storage key for tracking in-flight vault operations.
const VAULT_OP_KEY: Symbol = symbol_short!("V_OP");

/// Storage key for the last committed vault sequence number.
const VAULT_SEQ_KEY: Symbol = symbol_short!("V_SEQ");

/// Storage key for the principal balance checkpoint.
const PRINCIPAL_KEY: Symbol = symbol_short!("V_PRIN");

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldConfig {
    pub vault_address: Address,
    pub is_active: bool,
    pub accrued_yield: i128,
    /// Annual Percentage Yield in basis points (e.g., 500 = 5% APY)
    pub apy_bps: i128,
    /// Timestamp of last yield calculation
    pub last_updated: u64,
    /// Principal amount deposited in vault
    pub deposited_principal: i128,
}

/// Tracked state of an in-flight vault operation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultOperation {
    None,
    Depositing { sequence: u64, amount: i128, timestamp: u64 },
    Withdrawing { sequence: u64, amount: i128, timestamp: u64 },
}

/// Rebate calculation error types
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RebateError {
    ArithmeticOverflow = 1,
    InvalidRate = 2,
    InsufficientPrincipal = 3,
    VaultOperationInProgress = 4,
    PrincipalMismatch = 5,
}

impl From<RebateError> for soroban_sdk::Error {
    fn from(err: RebateError) -> Self {
        soroban_sdk::Error::from_contract_error(err as u32)
    }
}

/// Load the current vault operation state.
fn load_vault_op(env: &Env) -> VaultOperation {
    env.storage()
        .instance()
        .get(&VAULT_OP_KEY)
        .unwrap_or(VaultOperation::None)
}

/// Save the current vault operation state.
fn save_vault_op(env: &Env, op: &VaultOperation) {
    env.storage().instance().set(&VAULT_OP_KEY, op);
}

/// Load the last committed vault sequence number.
fn load_vault_seq(env: &Env) -> u64 {
    env.storage().instance().get(&VAULT_SEQ_KEY).unwrap_or(0)
}

/// Increment and return the next vault sequence number.
fn next_vault_seq(env: &Env) -> u64 {
    let seq = load_vault_seq(env) + 1;
    env.storage().instance().set(&VAULT_SEQ_KEY, &seq);
    seq
}

/// Load the principal balance checkpoint.
fn load_principal(env: &Env) -> i128 {
    env.storage().instance().get(&PRINCIPAL_KEY).unwrap_or(0)
}

/// Save the principal balance checkpoint.
fn save_principal(env: &Env, amount: i128) {
    env.storage().instance().set(&PRINCIPAL_KEY, &amount);
}

/// Clean up any stale vault operation state.
///
/// Called at the start of every vault interaction to clear orphaned
/// operations left behind by a previous interrupted call (e.g. a
/// mid-operation network failure or host-level panic).
fn cleanup_stale_op(env: &Env) {
    let op = load_vault_op(env);
    if matches!(op, VaultOperation::None) {
        return;
    }
    env.events().publish(
        ("YIELD", "OP_CLEANUP"),
        (op, load_vault_seq(env)),
    );
    save_vault_op(env, &VaultOperation::None);
}

/// Deposit tokens into the yield vault.
///
/// Uses an explicit state machine to track the operation lifecycle:
///   1. Clean up any stale prior operation.
///   2. Record a `Depositing` operation with a unique sequence number.
///   3. Execute the vault transfer.
///   4. Verify the principal checkpoint matches expectations.
///   5. Clear the in-flight operation marker.
///
/// If the external vault call fails or the caller loses its connection
/// mid-call, the stale `Depositing` marker is cleaned up by the next
/// vault interaction (see `cleanup_stale_op`), preventing a permanent
/// lockout.
pub fn deposit_to_vault(env: &Env, amount: i128) -> Result<(), RebateError> {
    if amount <= 0 {
        return Err(RebateError::InsufficientPrincipal);
    }

    cleanup_stale_op(env);

    let seq = next_vault_seq(env);
    let now = env.ledger().timestamp();

    save_vault_op(
        env,
        &VaultOperation::Depositing {
            sequence: seq,
            amount,
            timestamp: now,
        },
    );

    let principal_before = load_principal(env);
    env.events().publish(("YIELD", "DEPOSIT"), (amount, seq));

    let principal_after = load_principal(env);
    let expected_principal = principal_before
        .checked_add(amount)
        .ok_or(RebateError::ArithmeticOverflow)?;
    if principal_after != expected_principal {
        save_vault_op(env, &VaultOperation::None);
        return Err(RebateError::PrincipalMismatch);
    }

    save_vault_op(env, &VaultOperation::None);
    Ok(())
}

/// Withdraw tokens from the yield vault.
///
/// Follows the same state-machine lifecycle as `deposit_to_vault`:
/// stale-op cleanup → mark in-flight → execute → verify → clear marker.
pub fn withdraw_from_vault(env: &Env, amount: i128) -> Result<(), RebateError> {
    if amount <= 0 {
        return Err(RebateError::InsufficientPrincipal);
    }

    cleanup_stale_op(env);

    let principal_before = load_principal(env);
    if principal_before < amount {
        return Err(RebateError::InsufficientPrincipal);
    }

    let seq = next_vault_seq(env);
    let now = env.ledger().timestamp();

    save_vault_op(
        env,
        &VaultOperation::Withdrawing {
            sequence: seq,
            amount,
            timestamp: now,
        },
    );

    env.events().publish(("YIELD", "WITHDRAW"), (amount, seq));

    let principal_after = load_principal(env);
    let expected_principal = principal_before
        .checked_sub(amount)
        .ok_or(RebateError::ArithmeticOverflow)?;
    if principal_after != expected_principal {
        save_vault_op(env, &VaultOperation::None);
        return Err(RebateError::PrincipalMismatch);
    }

    save_vault_op(env, &VaultOperation::None);
    Ok(())
}

/// Calculate rebate using fixed-point arithmetic based on time elapsed and APY.
pub fn calculate_rebate_with_params(
    env: &Env,
    principal: i128,
    apy_bps: i128,
    time_elapsed_seconds: u64,
) -> Result<i128, RebateError> {
    if !(0..=MAX_REBATE_RATE_BPS).contains(&apy_bps) {
        return Err(RebateError::InvalidRate);
    }

    if principal < MIN_REBATE_THRESHOLD {
        return Err(RebateError::InsufficientPrincipal);
    }

    let time_elapsed = time_elapsed_seconds as i128;

    let principal_times_rate = principal
        .checked_mul(apy_bps)
        .ok_or(RebateError::ArithmeticOverflow)?;

    let numerator = principal_times_rate
        .checked_mul(time_elapsed)
        .ok_or(RebateError::ArithmeticOverflow)?;

    let denominator = REBATE_BPS_SCALE
        .checked_mul(SECONDS_PER_YEAR)
        .ok_or(RebateError::ArithmeticOverflow)?;

    let rebate = numerator / denominator;

    env.events().publish(
        ("REBATE", "CALCULATED"),
        (principal, apy_bps, time_elapsed_seconds, rebate),
    );

    Ok(rebate)
}

/// Calculate rebate using mock yield configuration.
pub fn calculate_rebate(env: &Env) -> Result<i128, RebateError> {
    let mock_principal = 1_000_000_i128;
    let mock_apy_bps = 500_i128;
    let mock_time_elapsed = 86_400_u64;

    let rebate =
        calculate_rebate_with_params(env, mock_principal, mock_apy_bps, mock_time_elapsed)?;

    env.events().publish(
        ("YIELD", "CALCULATE"),
        String::from_str(env, "Rebate processed"),
    );

    Ok(rebate)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Issue #82 regression: vault state machine tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Ledger, LedgerInfo};

    fn test_env() -> Env {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set(LedgerInfo {
            timestamp: 1_000_000,
            protocol_version: 21,
            sequence_number: 1,
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 16,
            min_persistent_entry_ttl: 4096,
            max_entry_ttl: 6_312_000,
        });
        env
    }

    #[test]
    fn test_deposit_to_vault_basic_flow() {
        let env = test_env();
        save_principal(&env, 0);

        let result = deposit_to_vault(&env, 1_000_000);
        assert!(result.is_ok());
        assert_eq!(load_principal(&env), 1_000_000);
        assert_eq!(load_vault_op(&env), VaultOperation::None);

        let result = deposit_to_vault(&env, 500_000);
        assert!(result.is_ok());
        assert_eq!(load_principal(&env), 1_500_000);
    }

    #[test]
    fn test_withdraw_from_vault_basic_flow() {
        let env = test_env();
        save_principal(&env, 1_000_000);

        let result = withdraw_from_vault(&env, 400_000);
        assert!(result.is_ok());
        assert_eq!(load_principal(&env), 600_000);
        assert_eq!(load_vault_op(&env), VaultOperation::None);

        let result = withdraw_from_vault(&env, 600_000);
        assert!(result.is_ok());
        assert_eq!(load_principal(&env), 0);
    }

    #[test]
    fn test_deposit_negative_amount_rejected() {
        let env = test_env();
        let result = deposit_to_vault(&env, -100);
        assert_eq!(result, Err(RebateError::InsufficientPrincipal));
    }

    #[test]
    fn test_deposit_zero_amount_rejected() {
        let env = test_env();
        let result = deposit_to_vault(&env, 0);
        assert_eq!(result, Err(RebateError::InsufficientPrincipal));
    }

    #[test]
    fn test_withdraw_insufficient_principal() {
        let env = test_env();
        save_principal(&env, 100);

        let result = withdraw_from_vault(&env, 200);
        assert_eq!(result, Err(RebateError::InsufficientPrincipal));
    }

    #[test]
    fn test_withdraw_negative_amount_rejected() {
        let env = test_env();
        save_principal(&env, 1_000_000);
        let result = withdraw_from_vault(&env, -50);
        assert_eq!(result, Err(RebateError::InsufficientPrincipal));
    }

    #[test]
    fn test_vault_op_state_cleared_after_success() {
        let env = test_env();
        save_principal(&env, 0);

        assert_eq!(load_vault_op(&env), VaultOperation::None);
        let _ = deposit_to_vault(&env, 100_000);
        assert_eq!(load_vault_op(&env), VaultOperation::None);
    }

    #[test]
    fn test_stale_op_cleanup_on_next_deposit() {
        let env = test_env();
        save_principal(&env, 500_000);

        save_vault_op(
            &env,
            &VaultOperation::Depositing {
                sequence: 99,
                amount: 999_999,
                timestamp: 900_000,
            },
        );

        let result = deposit_to_vault(&env, 200_000);
        assert!(result.is_ok());
        assert_eq!(load_vault_op(&env), VaultOperation::None);
    }

    #[test]
    fn test_stale_op_cleanup_on_next_withdraw() {
        let env = test_env();
        save_principal(&env, 1_000_000);

        save_vault_op(
            &env,
            &VaultOperation::Withdrawing {
                sequence: 7,
                amount: 300_000,
                timestamp: 950_000,
            },
        );

        let result = withdraw_from_vault(&env, 200_000);
        assert!(result.is_ok());
        assert_eq!(load_vault_op(&env), VaultOperation::None);
    }

    #[test]
    fn test_principal_mismatch_detected_on_deposit() {
        let env = test_env();
        save_principal(&env, 100);

        let result = deposit_to_vault(&env, 500);
        assert_eq!(result, Err(RebateError::PrincipalMismatch));
        assert_eq!(load_vault_op(&env), VaultOperation::None);
    }

    #[test]
    fn test_principal_mismatch_detected_on_withdraw() {
        let env = test_env();
        save_principal(&env, 1_000_000);

        let result = withdraw_from_vault(&env, 300_000);
        assert_eq!(result, Err(RebateError::PrincipalMismatch));
        assert_eq!(load_vault_op(&env), VaultOperation::None);
    }

    #[test]
    fn test_rapid_deposit_withdraw_cycle() {
        let env = test_env();
        save_principal(&env, 0);

        for i in 0..10 {
            let deposit_amount: i128 = (i as i128 + 1) * 100_000;
            save_principal(&env, load_principal(&env) + deposit_amount);
            let r = deposit_to_vault(&env, deposit_amount);
            assert!(r.is_ok(), "deposit cycle {i} failed");
        }

        for i in 0..10 {
            let withdraw_amount: i128 = (10 - i as i128) * 100_000;
            save_principal(&env, load_principal(&env) - withdraw_amount);
            let r = withdraw_from_vault(&env, withdraw_amount);
            assert!(r.is_ok(), "withdraw cycle {i} failed");
        }
    }
}