#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{self, StellarAssetClient},
    Address, Bytes, Env,
};

fn setup_credential_tests() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&user, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, user, client)
}

/// Test that invalid transitions are rejected through the public entry point
#[test]
fn test_invalid_transition_archived_to_active_rejected() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 1u64;

    // Initialize credential (Draft state)
    client.init_credential(&credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Draft
    );

    // Activate it
    let _ = client.activate_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Active
    );

    // Archive it
    let _ = client.archive_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Archived
    );

    // Try to activate it again - should fail (Archived is terminal)
    let err = client
        .try_activate_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);

    // State should remain Archived
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Archived
    );
}

/// Test that a revoked credential is rejected by verify_external_anchor
#[test]
fn test_revoked_credential_rejected_by_verify() {
    let (env, admin, _user, client) = setup_credential_tests();
    let credential_id = 1u64;

    // Initialize and activate credential
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);

    // Create an anchor for this credential
    let external_id = Bytes::from_array(&env, &[1, 2, 3, 4]);
    let system = Bytes::from_array(&env, b"test-sys");

    // Manually create the anchor using the internal function wrapped in contract context
    let contract_address = client.address.clone();
    let success = env.as_contract(&contract_address, || {
        credential_anchoring::create_credential_anchor(
            &env,
            credential_id,
            external_id.clone(),
            system.clone(),
        )
    });
    assert!(success, "anchor creation should succeed");

    // Verify the credential is found when Active
    let result = env.as_contract(&contract_address, || {
        credential_anchoring::verify_external_anchor(&env, &external_id, &system)
    });
    assert_eq!(result, Some(credential_id));

    // Now revoke the credential
    let _ = client.revoke_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Revoked
    );

    // Verify the credential is now rejected (returns None)
    let result = env.as_contract(&contract_address, || {
        credential_anchoring::verify_external_anchor(&env, &external_id, &system)
    });
    assert_eq!(result, None, "revoked credential should not be verified");
}

/// Test Draft -> Active -> Suspended -> Active cycle
#[test]
fn test_suspend_and_reactivate_workflow() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 2u64;

    // Initialize credential (Draft)
    client.init_credential(&credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Draft
    );

    // Activate
    let _ = client.activate_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Active
    );

    // Suspend
    let _ = client.suspend_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Suspended
    );

    // Reactivate from Suspended
    let _ = client.activate_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Active
    );
}

/// Test that suspended credentials are rejected by verify_external_anchor
#[test]
fn test_suspended_credential_rejected_by_verify() {
    let (env, admin, _user, client) = setup_credential_tests();
    let credential_id = 3u64;

    // Initialize, activate, and create anchor
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);

    let external_id = Bytes::from_array(&env, &[5, 6, 7, 8]);
    let system = Bytes::from_array(&env, b"kyc-v1");

    let contract_address = client.address.clone();
    let success = env.as_contract(&contract_address, || {
        credential_anchoring::create_credential_anchor(
            &env,
            credential_id,
            external_id.clone(),
            system.clone(),
        )
    });
    assert!(success);

    // Verify works when Active
    let result = env.as_contract(&contract_address, || {
        credential_anchoring::verify_external_anchor(&env, &external_id, &system)
    });
    assert_eq!(result, Some(credential_id));

    // Suspend the credential
    let _ = client.suspend_credential(&admin, &credential_id);

    // Verify should now return None
    let result = env.as_contract(&contract_address, || {
        credential_anchoring::verify_external_anchor(&env, &external_id, &system)
    });
    assert_eq!(result, None, "suspended credential should not be verified");
}

/// Test Active -> Expired -> Archived workflow
#[test]
fn test_expire_and_archive_workflow() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 4u64;

    // Initialize and activate
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);

    // Expire
    let _ = client.expire_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Expired
    );

    // Archive from Expired
    let _ = client.archive_credential(&admin, &credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Archived
    );
}

/// Test that Revoked is terminal - no transitions allowed
#[test]
fn test_revoked_is_terminal() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 5u64;

    // Initialize, activate, and revoke
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);
    let _ = client.revoke_credential(&admin, &credential_id);

    // Try to activate - should fail
    let err = client
        .try_activate_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);

    // Try to suspend - should fail
    let err = client
        .try_suspend_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);

    // Try to archive - should fail
    let err = client
        .try_archive_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);

    // State should remain Revoked
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Revoked
    );
}

/// Test that a revoked credential cannot be revived by re-initializing it
#[test]
fn test_revoked_credential_cannot_be_reinitialized() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 6u64;

    // Initialize, activate, and revoke
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);
    let _ = client.revoke_credential(&admin, &credential_id);

    // Attempt to re-initialize (would reset state to Draft) — must be a no-op
    client.init_credential(&credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Revoked,
        "re-initialization must not revive a revoked credential"
    );

    // Activating must still fail
    let err = client
        .try_activate_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);
}

/// Test that an archived credential cannot be revived by re-initializing it
#[test]
fn test_archived_credential_cannot_be_reinitialized() {
    let (_env, admin, _user, client) = setup_credential_tests();
    let credential_id = 7u64;

    // Initialize, activate, and archive
    client.init_credential(&credential_id);
    let _ = client.activate_credential(&admin, &credential_id);
    let _ = client.archive_credential(&admin, &credential_id);

    // Attempt to re-initialize — must be a no-op
    client.init_credential(&credential_id);
    assert_eq!(
        client.get_credential_state(&credential_id),
        credential_lifecycle::CredentialState::Archived,
        "re-initialization must not revive an archived credential"
    );

    // Activating must still fail
    let err = client
        .try_activate_credential(&admin, &credential_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidStateTransition);
}
