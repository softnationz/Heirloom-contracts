// Minimal oracle module for external release condition queries
use soroban_sdk::{Address, Env, Symbol, Val, Vec};

pub fn query(env: &Env, address: &Address) -> bool {
    // Expect the external oracle contract to expose a `query_release` function returning a
    // boolean indicating whether the release condition is met. This call may fail (e.g. the
    // oracle contract doesn't exist or panics); treat any failure as "condition not met" to
    // avoid unintended releases.
    let func = Symbol::new(env, "query_release");
    let args: Vec<Val> = Vec::new(env);
    let result = env.try_invoke_contract::<bool, soroban_sdk::Error>(address, &func, args);
    match result {
        Ok(Ok(b)) => b,
        _ => false,
    }
}
