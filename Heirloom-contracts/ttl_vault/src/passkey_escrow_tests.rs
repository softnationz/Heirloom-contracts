#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, Symbol, TryIntoVal, Val,
};

/// Returns true if any emitted event's first topic equals `topic`.
fn saw_topic(env: &Env, topic: Symbol) -> bool {
    env.events().all().iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val(env).ok())
            .is_some_and(|s: Symbol| s == topic)
    })
}

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
    let recovery_contact = Address::generate(&env);

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
    (env, owner, beneficiary, recovery_contact, client)
}

fn make_vault_with_passkey(
    client: &TtlVaultContractClient<'static>,
    env: &Env,
    owner: &Address,
    beneficiary: &Address,
) -> (u64, BytesN<32>) {
    let vault_id = client.create_vault(owner, beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::from_array(env, &[7u8; 32]);
    client.add_passkey(&vault_id, owner, &passkey_hash);
    (vault_id, passkey_hash)
}

// Requirement 3.1 / 3.12: escrow_passkey creates an Escrow_Record and emits pk_esc.
#[test]
fn test_escrow_passkey_creates_record_and_emits_event() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);

    let record = client.get_passkey_escrow(&vault_id, &passkey_hash).unwrap();
    assert_eq!(record.recovery_contact, recovery_contact);

    assert!(saw_topic(&env, PASSKEY_ESCROWED_TOPIC));
}

// Requirement 3.2: escrow_passkey requires owner authorization.
#[test]
fn test_escrow_passkey_requires_owner_auth() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    let stranger = Address::generate(&env);
    let err = client
        .try_escrow_passkey(&vault_id, &stranger, &passkey_hash, &recovery_contact)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

// Requirement 3.3: PasskeyNotFound if the hash isn't registered.
#[test]
fn test_escrow_passkey_not_found() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let passkey_hash = BytesN::from_array(&env, &[9u8; 32]);

    let err = client
        .try_escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::PasskeyNotFound);
}

// Requirement 3.4: InvalidBeneficiary if recovery_contact == owner.
#[test]
fn test_escrow_passkey_rejects_self_as_recovery_contact() {
    let (env, owner, beneficiary, _recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    let err = client
        .try_escrow_passkey(&vault_id, &owner, &passkey_hash, &owner)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidBeneficiary);
}

// Requirement 3.5: AlreadyInEscrow on double-escrow of the same passkey.
#[test]
fn test_escrow_passkey_rejects_double_escrow() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);
    let err = client
        .try_escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::AlreadyInEscrow);
}

// Requirement 3.6: owner cannot check in with an escrowed passkey.
#[test]
fn test_check_in_rejected_while_passkey_escrowed() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);

    let err = client
        .try_check_in(&vault_id, &owner, &passkey_hash, &0u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::PasskeyInEscrow);
}

// Requirement 3.7 / 3.13: recovery_contact releases escrow, restoring usability,
// and the pk_esc_rel event is emitted.
#[test]
fn test_release_escrow_restores_check_in_and_emits_event() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);
    client.release_escrow_passkey(&vault_id, &recovery_contact, &passkey_hash);

    assert!(client
        .get_passkey_escrow(&vault_id, &passkey_hash)
        .is_none());

    assert!(saw_topic(&env, PASSKEY_ESCROW_RELEASED_TOPIC));

    // Passkey is usable again.
    assert!(client
        .try_check_in(&vault_id, &owner, &passkey_hash, &0u64)
        .is_ok());
}

// Requirement 3.9: non-recovery-contact release attempt fails with NotRecoveryContact.
#[test]
fn test_release_escrow_requires_recovery_contact() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);
    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);

    let stranger = Address::generate(&env);
    let err = client
        .try_release_escrow_passkey(&vault_id, &stranger, &passkey_hash)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotRecoveryContact);
}

// Requirement 3.10 / 3.11 / 3.14: owner can cancel escrow, restoring usability,
// and the pk_esc_can event (topic truncated to 9 chars: "pk_esccan") is emitted.
#[test]
fn test_cancel_escrow_restores_check_in_and_emits_event() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);
    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);

    client.cancel_passkey_escrow(&vault_id, &owner, &passkey_hash);

    assert!(client
        .get_passkey_escrow(&vault_id, &passkey_hash)
        .is_none());

    assert!(saw_topic(&env, PASSKEY_ESCROW_CANCELLED_TOPIC));

    assert!(client
        .try_check_in(&vault_id, &owner, &passkey_hash, &0u64)
        .is_ok());
}

// cancel_passkey_escrow requires owner auth.
#[test]
fn test_cancel_escrow_requires_owner() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);
    client.escrow_passkey(&vault_id, &owner, &passkey_hash, &recovery_contact);

    let err = client
        .try_cancel_passkey_escrow(&vault_id, &recovery_contact, &passkey_hash)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

// Requirement 3.15: get_passkey_escrow returns None when no escrow exists.
#[test]
fn test_get_passkey_escrow_returns_none_when_absent() {
    let (env, owner, beneficiary, _recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    assert!(client
        .get_passkey_escrow(&vault_id, &passkey_hash)
        .is_none());
}

// release_escrow_passkey with no escrow record returns PasskeyNotFound.
#[test]
fn test_release_escrow_not_found() {
    let (env, owner, beneficiary, recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    let err = client
        .try_release_escrow_passkey(&vault_id, &recovery_contact, &passkey_hash)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::PasskeyNotFound);
}

// cancel_passkey_escrow with no escrow record returns PasskeyNotFound.
#[test]
fn test_cancel_escrow_not_found() {
    let (env, owner, beneficiary, _recovery_contact, client) = setup();
    let (vault_id, passkey_hash) = make_vault_with_passkey(&client, &env, &owner, &beneficiary);

    let err = client
        .try_cancel_passkey_escrow(&vault_id, &owner, &passkey_hash)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::PasskeyNotFound);
}
