use soroban_sdk::{contract, contracterror, contractimpl, Env};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    ProcessorLocked = 2001,
    CalculationOverflow = 2002,
    BatchTooLarge = 2003,
}

#[contract]
pub struct BatchTransferProcessor;

#[contractimpl]
impl BatchTransferProcessor {
    pub fn process_batch(env: Env, amounts: soroban_sdk::Vec<u64>) -> Result<u64, Error> {
        with_guard(&env, || {
            // boundary checks
            if amounts.len() > 100 {
                return Err(Error::BatchTooLarge);
            }

            let mut total: u64 = 0;
            for amount in amounts.iter() {
                // precision / error-boundary handlers
                match total.checked_add(amount) {
                    Some(new_total) => total = new_total,
                    None => {
                        return Err(Error::CalculationOverflow);
                    }
                }
            }

            Ok(total)
        })
    }
}

/// Execute `f` under the re-entrancy guard, releasing the lock afterwards.
///
/// Uses a depth counter stored at the `B_Lock` symbol key instead of a
/// boolean flag. See `drip_stream::state::with_guard` for the rationale.
const MAX_REENTRANCY_DEPTH: u32 = 1;

fn with_guard<R>(env: &Env, f: impl FnOnce() -> Result<R, Error>) -> Result<R, Error> {
    let lock_key = soroban_sdk::symbol_short!("B_Lock");
    let depth: u32 = env.storage().instance().get(&lock_key).unwrap_or(0);
    if depth >= MAX_REENTRANCY_DEPTH {
        return Err(Error::ProcessorLocked);
    }
    env.storage().instance().set(&lock_key, &(depth + 1));
    let result = f();
    let d: u32 = env.storage().instance().get(&lock_key).unwrap_or(1);
    if d > 0 {
        env.storage().instance().set(&lock_key, &(d - 1));
    }
    result
}
