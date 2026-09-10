#![cfg(test)]

use super::*;
use soroban_sdk::{bytes, testutils::Address as _, vec};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    let client = SbtContractClient::new(&env, &contract_id);
    client.initialize(&admin);

    (env, contract_id, owner)
}

// ---- generation & redemption ----

#[test]
fn generate_returns_plaintext_codes_and_redeem_reassigns_holder() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    assert_eq!(codes.len(), RECOVERY_CODE_COUNT);

    let new_holder = Address::generate(&env);
    let code: Bytes = codes.get(0).unwrap().into();
    assert!(client.recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder));
    assert_eq!(client.owner_of(&sbt_id), new_holder);
}

#[test]
fn redeem_with_wrong_code_returns_false_without_changing_holder() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    client.generate_sbt_recovery_codes(&sbt_id);

    let new_holder = Address::generate(&env);
    let wrong = bytes!(&env, 0x0102030405060708);
    assert!(!client.recover_sbt_with_recovery_code(&sbt_id, &wrong, &new_holder));
    assert_eq!(client.owner_of(&sbt_id), owner);
}

#[test]
fn redeemed_code_is_one_time() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let new_holder = Address::generate(&env);
    let code: Bytes = codes.get(0).unwrap().into();

    assert!(client.recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder));
    // The same code must not redeem a second time.
    assert!(!client.recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder));
}

#[test]
fn regenerating_codes_invalidates_previous_batch() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let first_batch = client.generate_sbt_recovery_codes(&sbt_id);
    let second_batch = client.generate_sbt_recovery_codes(&sbt_id);

    let new_holder = Address::generate(&env);
    let old_code: Bytes = first_batch.get(0).unwrap().into();
    assert!(!client.recover_sbt_with_recovery_code(&sbt_id, &old_code, &new_holder));

    let new_code: Bytes = second_batch.get(0).unwrap().into();
    assert!(client.recover_sbt_with_recovery_code(&sbt_id, &new_code, &new_holder));
    assert_eq!(client.owner_of(&sbt_id), new_holder);
}

#[test]
fn redeeming_without_generated_codes_panics_no_recovery_codes() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));

    let new_holder = Address::generate(&env);
    let err = client
        .try_recover_sbt_with_recovery_code(&sbt_id, &bytes!(&env, 0xdeadbeef), &new_holder)
        .unwrap_err();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(SbtError::NoRecoveryCodes as u32)
    );
}

#[test]
fn recovery_clears_active_delegation() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let code: Bytes = codes.get(0).unwrap().into();

    let delegate = Address::generate(&env);
    client.delegate_sbt_temporarily(&sbt_id, &delegate, &3_600u64);
    assert!(client.get_active_delegate(&sbt_id).is_some());

    let new_holder = Address::generate(&env);
    assert!(client.recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder));
    assert!(client.get_active_delegate(&sbt_id).is_none());
}

#[test]
fn recovery_is_blocked_for_fractionally_owned_sbt() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let code: Bytes = codes.get(0).unwrap().into();

    let other = Address::generate(&env);
    client.create_fractional_sbt(
        &sbt_id,
        &vec![&env, owner.clone(), other],
        &vec![&env, 5_000u64, 5_000u64],
    );

    let new_holder = Address::generate(&env);
    let err = client
        .try_recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder)
        .unwrap_err();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(SbtError::FractionalOwnershipExists as u32)
    );
}

// ---- authorization ----

#[test]
#[should_panic]
fn generate_requires_owner_auth() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));

    env.set_auths(&[]);
    client.generate_sbt_recovery_codes(&sbt_id);
}

#[test]
#[should_panic]
fn redeem_requires_new_holder_auth() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let code: Bytes = codes.get(0).unwrap().into();

    let new_holder = Address::generate(&env);
    env.set_auths(&[]);
    client.recover_sbt_with_recovery_code(&sbt_id, &code, &new_holder);
}

// ---- rate limiting (issue #51) ----

/// An attacker guessing codes is bounded to RECOVERY_MAX_ATTEMPTS attempts
/// per window: after the budget is exhausted, even a valid code is rejected
/// and the holder is unchanged.
#[test]
fn rate_limit_bounds_attempts_per_window() {
    let (env, contract_id, owner) = setup();
    env.ledger().set_timestamp(1_000);
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let valid_code: Bytes = codes.get(0).unwrap().into();
    let new_holder = Address::generate(&env);

    for _ in 0..RECOVERY_MAX_ATTEMPTS {
        assert!(!client.recover_sbt_with_recovery_code(
            &sbt_id,
            &bytes!(&env, 0xffffffff),
            &new_holder,
        ));
    }

    let err = client
        .try_recover_sbt_with_recovery_code(&sbt_id, &valid_code, &new_holder)
        .unwrap_err();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(SbtError::RecoveryRateLimited as u32)
    );
    assert_eq!(client.owner_of(&sbt_id), owner);
}

/// Once the window elapses, the attempt budget resets and a valid code
/// redeems again.
#[test]
fn rate_limit_window_resets_after_elapsed_time() {
    let (env, contract_id, owner) = setup();
    env.ledger().set_timestamp(1_000);
    let client = SbtContractClient::new(&env, &contract_id);
    let sbt_id = client.mint(&owner, &String::from_str(&env, "credential"));
    let codes = client.generate_sbt_recovery_codes(&sbt_id);
    let valid_code: Bytes = codes.get(0).unwrap().into();
    let new_holder = Address::generate(&env);

    for _ in 0..RECOVERY_MAX_ATTEMPTS {
        assert!(!client.recover_sbt_with_recovery_code(
            &sbt_id,
            &bytes!(&env, 0xffffffff),
            &new_holder,
        ));
    }
    assert!(client
        .try_recover_sbt_with_recovery_code(&sbt_id, &valid_code, &new_holder)
        .is_err());

    env.ledger()
        .set_timestamp(1_000 + RECOVERY_ATTEMPT_WINDOW_SECONDS);
    assert!(client.recover_sbt_with_recovery_code(&sbt_id, &valid_code, &new_holder));
    assert_eq!(client.owner_of(&sbt_id), new_holder);
}
