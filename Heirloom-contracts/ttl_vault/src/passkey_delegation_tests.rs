#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, TryIntoVal, Val,
};

fn setup() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
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
    (env, owner, beneficiary, client)
}

/// AC1/AC10/AC12: delegating a registered passkey stores the delegation, it can
/// be queried back, and the `pk_del` event is emitted.
#[test]
fn test_delegate_passkey_creates_delegation_and_emits_event() {
    let (env, owner, beneficiary, client) = setup();
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 1000;
    client.delegate_passkey(&id, &owner, &passkey, &delegate, &expires_at);

    let delegation = client.get_passkey_delegation(&id, &passkey).unwrap();
    assert_eq!(delegation.delegate, delegate);
    assert_eq!(delegation.expires_at, expires_at);

    let events = env.events().all();
    let saw_delegated = events.iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val(&env).ok())
            .is_some_and(|s: soroban_sdk::Symbol| s == PASSKEY_DELEGATED_TOPIC)
    });
    assert!(saw_delegated, "pk_del event should be emitted");
}

/// AC2: only the vault owner may create a delegation.
#[test]
fn test_delegate_passkey_requires_owner_auth() {
    let (env, owner, beneficiary, client) = setup();
    let stranger = Address::generate(&env);
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 1000;
    let err = client
        .try_delegate_passkey(&id, &stranger, &passkey, &delegate, &expires_at)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// AC3: delegating a passkey that was never registered fails with PasskeyNotFound.
#[test]
fn test_delegate_passkey_unregistered_hash_fails() {
    let (env, owner, beneficiary, client) = setup();
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);

    let expires_at = env.ledger().timestamp() + 1000;
    let err = client
        .try_delegate_passkey(&id, &owner, &passkey, &delegate, &expires_at)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::PasskeyNotFound);
}

/// AC4: an `expires_at` in the past (or now) is rejected.
#[test]
fn test_delegate_passkey_past_expiry_fails() {
    let (env, owner, beneficiary, client) = setup();
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let now = env.ledger().timestamp();
    let err = client
        .try_delegate_passkey(&id, &owner, &passkey, &delegate, &now)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidInterval);
}

/// AC5: an owner cannot delegate a passkey to themselves.
#[test]
fn test_delegate_passkey_self_delegation_fails() {
    let (env, owner, beneficiary, client) = setup();
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 1000;
    let err = client
        .try_delegate_passkey(&id, &owner, &passkey, &owner, &expires_at)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidBeneficiary);
}

/// AC6: a delegate with a valid delegation may check in on the owner's behalf.
#[test]
fn test_delegate_can_check_in() {
    let (env, owner, beneficiary, client) = setup();
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 1000;
    client.delegate_passkey(&id, &owner, &passkey, &delegate, &expires_at);

    env.ledger().with_mut(|l| l.timestamp += 100);
    client.check_in(&id, &delegate, &passkey, &0u64);
    assert_eq!(
        client.get_vault(&id).last_check_in,
        env.ledger().timestamp()
    );
}

/// AC7: once `expires_at` has passed, the delegate can no longer check in.
#[test]
fn test_expired_delegation_rejects_check_in() {
    let (env, owner, beneficiary, client) = setup();
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 100;
    client.delegate_passkey(&id, &owner, &passkey, &delegate, &expires_at);

    env.ledger().with_mut(|l| l.timestamp = expires_at);
    let err = client
        .try_check_in(&id, &delegate, &passkey, &0u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// AC8/AC9/AC11: revocation removes the delegation, requires owner auth, and
/// emits `pk_del_rev`.
#[test]
fn test_revoke_passkey_delegation() {
    let (env, owner, beneficiary, client) = setup();
    let stranger = Address::generate(&env);
    let delegate = Address::generate(&env);
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    let expires_at = env.ledger().timestamp() + 1000;
    client.delegate_passkey(&id, &owner, &passkey, &delegate, &expires_at);

    // Non-owner cannot revoke.
    let err = client
        .try_revoke_passkey_delegation(&id, &stranger, &passkey)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);

    client.revoke_passkey_delegation(&id, &owner, &passkey);
    assert!(client.get_passkey_delegation(&id, &passkey).is_none());

    let events = env.events().all();
    let saw_revoked = events.iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val(&env).ok())
            .is_some_and(|s: soroban_sdk::Symbol| s == PASSKEY_DELEGATION_REVOKED_TOPIC)
    });
    assert!(saw_revoked, "pk_del_rev event should be emitted");

    // Revoked delegate can no longer check in.
    env.ledger().with_mut(|l| l.timestamp += 100);
    let err = client
        .try_check_in(&id, &delegate, &passkey, &0u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// AC12: querying a delegation that was never created returns `None`.
#[test]
fn test_get_passkey_delegation_returns_none_when_absent() {
    let (env, owner, beneficiary, client) = setup();
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    assert!(client.get_passkey_delegation(&id, &passkey).is_none());
}

/// Revoking a delegation that doesn't exist is a harmless no-op for the owner.
#[test]
fn test_revoke_nonexistent_delegation_is_noop() {
    let (env, owner, beneficiary, client) = setup();
    let passkey = BytesN::from_array(&env, &[1u8; 32]);
    let id = client.create_vault(&owner, &beneficiary, &3600u64, &None);
    client.add_passkey(&id, &owner, &passkey);

    client.revoke_passkey_delegation(&id, &owner, &passkey);
}
