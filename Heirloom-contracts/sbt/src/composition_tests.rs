#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, vec};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    SbtContractClient::new(&env, &contract_id).initialize(&admin);
    (env, contract_id, owner)
}

fn mint(client: &SbtContractClient, owner: &Address) -> u64 {
    client.mint(owner, &String::from_str(client.env(), "token"))
}

#[test]
fn composition_resolution_is_bounded_and_round_trips() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let root = mint(&client, &owner);
    let child = mint(&client, &owner);
    let leaf = mint(&client, &owner);

    client.set_composition_components(&root, &vec![&env, child]);
    client.set_composition_components(&child, &vec![&env, leaf]);

    assert_eq!(client.resolve_composition(&root), vec![&env, child, leaf]);
}

#[test]
fn composition_rejects_self_and_indirect_cycles() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let first = mint(&client, &owner);
    let second = mint(&client, &owner);

    assert!(client
        .try_set_composition_components(&first, &vec![&env, first])
        .is_err());
    client.set_composition_components(&first, &vec![&env, second]);
    assert!(client
        .try_set_composition_components(&second, &vec![&env, first])
        .is_err());
}
