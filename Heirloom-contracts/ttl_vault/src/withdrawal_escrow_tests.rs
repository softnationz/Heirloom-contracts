#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::StellarAssetClient,
    Address, Env,
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
    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, admin, client)
}

// Issue #261: a non-owner, non-authorized caller must not be able to create a
// withdrawal escrow, and no storage write may occur as a side effect of the
// rejected call.
#[test]
fn test_create_withdrawal_escrow_requires_owner() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    client.deposit(&vault_id, &owner, &100_000i128);

    let attacker = Address::generate(&env);
    let err = client
        .try_create_withdrawal_escrow(&vault_id, &50_000i128, &attacker, &attacker)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);

    // Prove no escrow was written: the legitimate beneficiary trying to verify
    // must see VaultNotFound (no escrow record exists), not any other error.
    let verify_err = client
        .try_verify_withdrawal_escrow(&vault_id, &beneficiary)
        .unwrap_err()
        .unwrap();
    assert_eq!(verify_err, ContractError::VaultNotFound);

    // Vault balance must be untouched by the rejected call.
    assert_eq!(client.get_vault_balance(&vault_id), 100_000i128);
}

#[test]
fn test_create_withdrawal_escrow_succeeds_for_owner() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    client.deposit(&vault_id, &owner, &100_000i128);

    client.create_withdrawal_escrow(&vault_id, &50_000i128, &beneficiary, &owner);

    assert!(saw_topic(&env, WITHDRAWAL_ESCROW_CREATED_TOPIC));
    // Escrow creation does not itself move funds out of the vault balance.
    assert_eq!(client.get_vault_balance(&vault_id), 100_000i128);
}

// Issue #261: even if an escrow is validly created, an unrelated address must
// not be able to trigger the release by calling verify_withdrawal_escrow.
#[test]
fn test_verify_withdrawal_escrow_requires_beneficiary() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    client.deposit(&vault_id, &owner, &100_000i128);
    client.create_withdrawal_escrow(&vault_id, &50_000i128, &beneficiary, &owner);

    let stranger = Address::generate(&env);
    let err = client
        .try_verify_withdrawal_escrow(&vault_id, &stranger)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotBeneficiary);

    // No transfer should have occurred: balance is unchanged and the escrow
    // is still verifiable by the real beneficiary afterwards.
    assert_eq!(client.get_vault_balance(&vault_id), 100_000i128);
}

#[test]
fn test_verify_withdrawal_escrow_succeeds_for_beneficiary() {
    let (env, owner, beneficiary, _admin, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    client.deposit(&vault_id, &owner, &100_000i128);
    client.create_withdrawal_escrow(&vault_id, &50_000i128, &beneficiary, &owner);

    client.verify_withdrawal_escrow(&vault_id, &beneficiary);

    assert!(saw_topic(&env, WITHDRAWAL_ESCROW_VERIFIED_TOPIC));
    assert_eq!(client.get_vault_balance(&vault_id), 50_000i128);

    // The escrow record is removed after verification, so a second attempt
    // must fail with VaultNotFound rather than allowing a double release.
    let err = client
        .try_verify_withdrawal_escrow(&vault_id, &beneficiary)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::VaultNotFound);
}

fn saw_topic(env: &Env, topic: soroban_sdk::Symbol) -> bool {
    use soroban_sdk::{IntoVal, TryIntoVal, Val};
    env.events().all().iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val(env).ok())
            .is_some_and(|s: soroban_sdk::Symbol| s == topic)
    })
}
