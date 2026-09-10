#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, TryIntoVal, Val,
};

fn setup() -> (
    Env,
    Address,
    Address,
    Address,
    TtlVaultContractClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, token_address, client)
}

/// Returns the `seconds_remaining` payload of the last `pk_expwrn` event for
/// `passkey_hash`, if one was emitted.
fn passkey_expiry_warning_seconds(env: &Env, passkey_hash: &BytesN<32>) -> Option<u64> {
    let events = env.events().all();
    for e in events.iter() {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(env);
        if let Some(Ok(sym)) = topics.get(0).map(|t| t.try_into_val(env)) {
            let s: soroban_sdk::Symbol = sym;
            if s == PASSKEY_EXPIRY_WARNING_TOPIC {
                let data: (BytesN<32>, u64) = e.2.clone().into_val(env);
                if &data.0 == passkey_hash {
                    return Some(data.1);
                }
            }
        }
    }
    None
}

#[test]
fn test_ping_expiry_emits_warning_for_passkey_near_expiry() {
    let (env, owner, beneficiary, _token_address, client) = setup();
    let interval = 1_000_000u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);

    let passkey_hash = BytesN::<32>::from_array(&env, &[7u8; 32]);
    client.add_passkey(&vault_id, &owner, &passkey_hash);

    // Expire the passkey in 100 seconds — well under EXPIRY_WARNING_THRESHOLD (24h).
    let now = env.ledger().timestamp();
    let expires_at = now + 100;
    client.extend_passkey_expiry(&vault_id, &owner, &passkey_hash, &expires_at);

    client.ping_expiry(&vault_id);

    let seconds_remaining = passkey_expiry_warning_seconds(&env, &passkey_hash);
    assert_eq!(seconds_remaining, Some(100));
}

#[test]
fn test_ping_expiry_no_warning_for_passkey_far_from_expiry() {
    let (env, owner, beneficiary, _token_address, client) = setup();
    let interval = 10_000_000u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);

    let passkey_hash = BytesN::<32>::from_array(&env, &[8u8; 32]);
    client.add_passkey(&vault_id, &owner, &passkey_hash);

    // Expiry is well beyond EXPIRY_WARNING_THRESHOLD (24h) away.
    let now = env.ledger().timestamp();
    let expires_at = now + EXPIRY_WARNING_THRESHOLD + 1_000;
    client.extend_passkey_expiry(&vault_id, &owner, &passkey_hash, &expires_at);

    client.ping_expiry(&vault_id);

    assert_eq!(passkey_expiry_warning_seconds(&env, &passkey_hash), None);
}

#[test]
fn test_ping_expiry_seconds_remaining_matches_expiry_delta() {
    let (env, owner, beneficiary, _token_address, client) = setup();
    let interval = 1_000_000u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);

    let passkey_hash = BytesN::<32>::from_array(&env, &[9u8; 32]);
    client.add_passkey(&vault_id, &owner, &passkey_hash);

    let now = env.ledger().timestamp();
    let expires_at = now + 42_000;
    client.extend_passkey_expiry(&vault_id, &owner, &passkey_hash, &expires_at);

    client.ping_expiry(&vault_id);

    assert_eq!(
        passkey_expiry_warning_seconds(&env, &passkey_hash),
        Some(42_000)
    );
}

#[test]
fn test_ping_expiry_does_not_warn_for_passkey_without_expiry_configured() {
    let (env, owner, beneficiary, _token_address, client) = setup();
    let interval = 1_000_000u64;
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);

    // No expiry configured for this passkey via extend_passkey_expiry.
    let passkey_hash = BytesN::<32>::from_array(&env, &[10u8; 32]);
    client.add_passkey(&vault_id, &owner, &passkey_hash);

    client.ping_expiry(&vault_id);

    assert_eq!(passkey_expiry_warning_seconds(&env, &passkey_hash), None);
}
