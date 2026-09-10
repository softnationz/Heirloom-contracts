#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, vec};

fn setup() -> (Env, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    let client = SbtContractClient::new(&env, &contract_id);
    client.initialize(&admin);

    (env, contract_id, owner, admin)
}

fn create_escrowed_credential(
    env: &Env,
    contract_id: &Address,
    owner: &Address,
    escrow_agent: &Address,
) -> u64 {
    let client = SbtContractClient::new(env, contract_id);
    let credential_id = client.mint(owner, &String::from_str(env, "credential"));
    client.escrow_sbt(
        &credential_id,
        escrow_agent,
        &Bytes::from_slice(env, b"condition"),
    );
    credential_id
}

#[test]
fn atomic_release_releases_every_credential_in_input_order() {
    let (env, contract_id, owner, _) = setup();
    let escrow_agent = Address::generate(&env);
    let first = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let second = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let client = SbtContractClient::new(&env, &contract_id);

    let results = client.atomic_release_credentials(&vec![&env, first, second]);

    assert_eq!(results, vec![&env, true, true]);
    assert!(client.get_escrow_status(&first).unwrap().released);
    assert!(client.get_escrow_status(&second).unwrap().released);
}

#[test]
fn atomic_release_rejects_empty_batches() {
    let (env, contract_id, _, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);

    assert!(client.try_atomic_release_credentials(&vec![&env]).is_err());
}

#[test]
fn atomic_release_rolls_back_when_any_credential_is_not_in_escrow() {
    let (env, contract_id, owner, _) = setup();
    let escrow_agent = Address::generate(&env);
    let escrowed = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let missing = escrowed + 1;
    let client = SbtContractClient::new(&env, &contract_id);

    assert!(client
        .try_atomic_release_credentials(&vec![&env, escrowed, missing])
        .is_err());
    assert!(!client.get_escrow_status(&escrowed).unwrap().released);
}

#[test]
fn atomic_release_rolls_back_when_any_credential_was_already_released() {
    let (env, contract_id, owner, _) = setup();
    let escrow_agent = Address::generate(&env);
    let pending = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let released = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let client = SbtContractClient::new(&env, &contract_id);
    client.release_sbt_from_escrow(&released, &Bytes::from_slice(&env, b"proof"));

    assert!(client
        .try_atomic_release_credentials(&vec![&env, pending, released])
        .is_err());
    assert!(!client.get_escrow_status(&pending).unwrap().released);
    assert!(client.get_escrow_status(&released).unwrap().released);
}

#[test]
fn atomic_release_rejects_duplicate_credential_ids_without_mutation() {
    let (env, contract_id, owner, _) = setup();
    let escrow_agent = Address::generate(&env);
    let credential_id = create_escrowed_credential(&env, &contract_id, &owner, &escrow_agent);
    let client = SbtContractClient::new(&env, &contract_id);

    assert!(client
        .try_atomic_release_credentials(&vec![&env, credential_id, credential_id])
        .is_err());
    assert!(!client.get_escrow_status(&credential_id).unwrap().released);
}

#[test]
fn atomic_release_requires_every_distinct_escrow_agent() {
    let (env, contract_id, owner, _) = setup();
    let first_agent = Address::generate(&env);
    let second_agent = Address::generate(&env);
    let first = create_escrowed_credential(&env, &contract_id, &owner, &first_agent);
    let second = create_escrowed_credential(&env, &contract_id, &owner, &second_agent);
    let client = SbtContractClient::new(&env, &contract_id);
    env.set_auths(&[]);

    assert!(client
        .try_atomic_release_credentials(&vec![&env, first, second])
        .is_err());
    assert!(!client.get_escrow_status(&first).unwrap().released);
    assert!(!client.get_escrow_status(&second).unwrap().released);
}
