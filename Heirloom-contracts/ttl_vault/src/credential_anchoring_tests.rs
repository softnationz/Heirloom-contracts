#[cfg(test)]
mod tests {
    use crate::credential_anchoring::*;
    use crate::credential_lifecycle;
    use crate::TtlVaultContract;
    use soroban_sdk::{Bytes, Env};

    #[test]
    fn test_create_and_verify_anchor() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TtlVaultContract);

        let credential_id = 42u64;
        let external_id = Bytes::from_slice(&env, b"external-id-123");
        let system = Bytes::from_slice(&env, b"kyc-v1");

        env.as_contract(&contract_id, || {
            // Initialize credential and activate it
            credential_lifecycle::init_credential_state(&env, credential_id);
            credential_lifecycle::transition_credential_state(
                &env,
                credential_id,
                credential_lifecycle::CredentialState::Active,
            );

            // Create anchor
            let success =
                create_credential_anchor(&env, credential_id, external_id.clone(), system.clone());
            assert!(success, "Anchor creation should succeed");

            // Verify anchor
            let result = verify_external_anchor(&env, &external_id, &system);
            assert_eq!(
                result,
                Some(credential_id),
                "Should retrieve correct credential ID"
            );

            // Check existence
            let exists = anchor_exists(&env, &external_id, &system);
            assert!(exists, "Anchor should exist");
        });
    }

    #[test]
    fn test_duplicate_anchor_rejected() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TtlVaultContract);

        let credential_id = 1u64;
        let external_id = Bytes::from_slice(&env, b"external-id-123");
        let system = Bytes::from_slice(&env, b"kyc-v1");

        env.as_contract(&contract_id, || {
            // Initialize and activate credential
            credential_lifecycle::init_credential_state(&env, credential_id);
            credential_lifecycle::transition_credential_state(
                &env,
                credential_id,
                credential_lifecycle::CredentialState::Active,
            );

            // Create anchor
            let success1 =
                create_credential_anchor(&env, credential_id, external_id.clone(), system.clone());
            assert!(success1, "First anchor creation should succeed");

            // Try to create duplicate
            let success2 =
                create_credential_anchor(&env, credential_id, external_id.clone(), system.clone());
            assert!(!success2, "Duplicate anchor should be rejected");
        });
    }

    #[test]
    fn test_remove_anchor() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TtlVaultContract);

        let credential_id = 42u64;
        let external_id = Bytes::from_slice(&env, b"external-id-123");
        let system = Bytes::from_slice(&env, b"kyc-v1");

        env.as_contract(&contract_id, || {
            // Initialize and activate credential
            credential_lifecycle::init_credential_state(&env, credential_id);
            credential_lifecycle::transition_credential_state(
                &env,
                credential_id,
                credential_lifecycle::CredentialState::Active,
            );

            // Create anchor
            let _ =
                create_credential_anchor(&env, credential_id, external_id.clone(), system.clone());
            assert!(
                anchor_exists(&env, &external_id, &system),
                "Anchor should exist"
            );

            // Remove anchor
            let success = remove_credential_anchor(&env, credential_id, &external_id, &system);
            assert!(success, "Anchor removal should succeed");

            // Verify it's gone
            let result = verify_external_anchor(&env, &external_id, &system);
            assert_eq!(result, None, "Anchor should be gone");
        });
    }

    #[test]
    fn test_multiple_anchors_per_credential() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TtlVaultContract);

        let credential_id = 42u64;
        let external_id_1 = Bytes::from_slice(&env, b"external-id-1");
        let external_id_2 = Bytes::from_slice(&env, b"external-id-2");
        let system_1 = Bytes::from_slice(&env, b"kyc-v1");
        let system_2 = Bytes::from_slice(&env, b"gov-id");

        env.as_contract(&contract_id, || {
            // Initialize and activate credential
            credential_lifecycle::init_credential_state(&env, credential_id);
            credential_lifecycle::transition_credential_state(
                &env,
                credential_id,
                credential_lifecycle::CredentialState::Active,
            );

            // Create multiple anchors
            let success1 = create_credential_anchor(
                &env,
                credential_id,
                external_id_1.clone(),
                system_1.clone(),
            );
            let success2 = create_credential_anchor(
                &env,
                credential_id,
                external_id_2.clone(),
                system_2.clone(),
            );

            assert!(success1, "First anchor should succeed");
            assert!(success2, "Second anchor should succeed");

            // Get all anchors
            let anchors = get_credential_anchors(&env, credential_id);
            assert_eq!(anchors.len(), 2, "Should have 2 anchors");
        });
    }

    #[test]
    fn test_anchor_counter() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TtlVaultContract);

        env.as_contract(&contract_id, || {
            let initial_count = get_anchor_count(&env);
            assert_eq!(initial_count, 0, "Initial count should be 0");

            // Initialize and activate credential
            let credential_id = 1u64;
            credential_lifecycle::init_credential_state(&env, credential_id);
            credential_lifecycle::transition_credential_state(
                &env,
                credential_id,
                credential_lifecycle::CredentialState::Active,
            );

            // Create an anchor
            let external_id = Bytes::from_slice(&env, b"test-id");
            let system = Bytes::from_slice(&env, b"test-sys");
            let _ = create_credential_anchor(&env, credential_id, external_id, system);

            let count_after = get_anchor_count(&env);
            assert_eq!(count_after, 1, "Count should increment");
        });
    }
}

/// Tests that exercise the `#[contractimpl]` entry points on
/// `TtlVaultContract` (via `TtlVaultContractClient`) rather than calling into
/// `credential_anchoring` directly, proving the feature is reachable
/// end-to-end (Issue #265).
#[cfg(test)]
mod contract_wiring_tests {
    use crate::{ContractError, TtlVaultContract, TtlVaultContractClient};
    use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};

    fn setup() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let xlm_token = Address::generate(&env);
        let contract_address = env.register_contract(None, TtlVaultContract);
        let client = TtlVaultContractClient::new(&env, &contract_address);
        client.initialize(&xlm_token, &admin);

        let caller = Address::generate(&env);
        (env, caller, admin, client)
    }

    #[test]
    fn test_create_verify_and_get_via_entry_points() {
        let (env, caller, admin, client) = setup();
        let credential_id = 7u64;
        let external_id = Bytes::from_slice(&env, b"external-id-abc");
        let system = Bytes::from_slice(&env, b"kyc-v1");

        client.init_credential(&credential_id);
        client.activate_credential(&admin, &credential_id);
        client.create_credential_anchor(&caller, &credential_id, &external_id, &system);

        let found = client.verify_external_anchor(&external_id, &system);
        assert_eq!(found, Some(credential_id));

        let anchors = client.get_credential_anchors(&credential_id);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors.get(0).unwrap().external_id, external_id);
        assert_eq!(anchors.get(0).unwrap().system, system);
    }

    #[test]
    fn test_remove_via_entry_point() {
        let (env, caller, _admin, client) = setup();
        let credential_id = 8u64;
        let external_id = Bytes::from_slice(&env, b"external-id-def");
        let system = Bytes::from_slice(&env, b"gov-id");

        client.create_credential_anchor(&caller, &credential_id, &external_id, &system);
        client.remove_credential_anchor(&caller, &credential_id, &external_id, &system);

        assert_eq!(client.verify_external_anchor(&external_id, &system), None);
    }

    #[test]
    fn test_remove_nonexistent_anchor_rejected() {
        let (env, caller, _admin, client) = setup();
        let external_id = Bytes::from_slice(&env, b"never-created");
        let system = Bytes::from_slice(&env, b"kyc-v1");

        let err = client
            .try_remove_credential_anchor(&caller, &1u64, &external_id, &system)
            .unwrap_err()
            .unwrap();
        assert_eq!(err, ContractError::AnchorNotFound);
    }

    #[test]
    fn test_duplicate_anchor_rejected_via_entry_point() {
        let (env, caller, _admin, client) = setup();
        let credential_id = 9u64;
        let external_id = Bytes::from_slice(&env, b"external-id-dup");
        let system = Bytes::from_slice(&env, b"hr-db");

        client.create_credential_anchor(&caller, &credential_id, &external_id, &system);

        let err = client
            .try_create_credential_anchor(&caller, &credential_id, &external_id, &system)
            .unwrap_err()
            .unwrap();
        assert_eq!(err, ContractError::AnchorAlreadyExists);
    }

    #[test]
    fn test_empty_external_id_rejected() {
        let (env, caller, _admin, client) = setup();
        let empty_external_id = Bytes::new(&env);
        let system = Bytes::from_slice(&env, b"kyc-v1");

        let err = client
            .try_create_credential_anchor(&caller, &1u64, &empty_external_id, &system)
            .unwrap_err()
            .unwrap();
        assert_eq!(err, ContractError::InvalidExternalId);
    }

    #[test]
    fn test_empty_system_rejected() {
        let (env, caller, _admin, client) = setup();
        let external_id = Bytes::from_slice(&env, b"external-id-xyz");
        let empty_system = Bytes::new(&env);

        let err = client
            .try_create_credential_anchor(&caller, &1u64, &external_id, &empty_system)
            .unwrap_err()
            .unwrap();
        assert_eq!(err, ContractError::InvalidAnchorSystem);
    }

    #[test]
    fn test_oversized_system_rejected() {
        let (env, caller, _admin, client) = setup();
        let external_id = Bytes::from_slice(&env, b"external-id-xyz");
        let oversized_system = Bytes::from_slice(&env, &[b'a'; 65]);

        let err = client
            .try_create_credential_anchor(&caller, &1u64, &external_id, &oversized_system)
            .unwrap_err()
            .unwrap();
        assert_eq!(err, ContractError::InvalidAnchorSystem);
    }

    #[test]
    #[should_panic]
    fn test_unauthorized_caller_cannot_create_anchor() {
        let (env, caller, _admin, client) = setup();
        // Withdraw the mocked authorization: no address has signed this
        // invocation, so `caller.require_auth()` inside the entry point must
        // reject the call. This proves a caller cannot anchor a credential
        // without authorizing the call themselves.
        env.set_auths(&[]);

        let external_id = Bytes::from_slice(&env, b"external-id-unauth");
        let system = Bytes::from_slice(&env, b"kyc-v1");
        client.create_credential_anchor(&caller, &2u64, &external_id, &system);
    }

    #[test]
    #[should_panic]
    fn test_unauthorized_caller_cannot_remove_anchor() {
        let (env, caller, _admin, client) = setup();
        let external_id = Bytes::from_slice(&env, b"external-id-unauth-2");
        let system = Bytes::from_slice(&env, b"kyc-v1");
        client.create_credential_anchor(&caller, &3u64, &external_id, &system);

        env.set_auths(&[]);
        client.remove_credential_anchor(&caller, &3u64, &external_id, &system);
    }
}
