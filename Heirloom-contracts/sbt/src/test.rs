#![cfg(test)]

use super::*;
use soroban_sdk::{bytes, testutils::Address as _};

fn setup() -> (Env, Address, Address, SbtContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let id = env.register_contract(None, SbtContract);
    let client = SbtContractClient::new(&env, &id);
    client.initialize(&admin);
    let client: SbtContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, owner, client)
}

/// Directly overwrites a minted SBT's schema_version/metadata in storage,
/// bypassing mint_sbt (which always issues at CURRENT_SCHEMA_VERSION), so
/// migrations away from older schema versions can be exercised.
fn force_schema_version(env: &Env, contract_id: &Address, sbt_id: u64, version: u32, metadata: Bytes) {
    env.as_contract(contract_id, || {
        let mut sbt: Sbt = env.storage().persistent().get(&DataKey::Sbt(sbt_id)).unwrap();
        sbt.schema_version = version;
        sbt.metadata = metadata;
        SbtContract::save_sbt(env, sbt_id, &sbt);
    });
}

// ---- issue #50: metadata schema versioning ----

/// A freshly minted SBT is already at CURRENT_SCHEMA_VERSION; there is no
/// forward version left to migrate to.
#[test]
fn test_migrate_at_current_version_is_noop() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    assert_eq!(client.get_schema_version(&sbt_id), CURRENT_SCHEMA_VERSION);
    assert!(!client.migrate_sbt_metadata(&sbt_id, &CURRENT_SCHEMA_VERSION));
    assert_eq!(client.get_schema_version(&sbt_id), CURRENT_SCHEMA_VERSION);
}

/// Requesting a version beyond CURRENT_SCHEMA_VERSION is rejected, not
/// silently clamped.
#[test]
fn test_migrate_beyond_current_version_returns_false() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);

    assert!(!client.migrate_sbt_metadata(&sbt_id, &(CURRENT_SCHEMA_VERSION + 1)));
}

/// Requesting a version at or below the SBT's current version is a no-op,
/// not a downgrade.
#[test]
fn test_migrate_to_lower_version_returns_false() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);
    force_schema_version(&env, &client.address, sbt_id, 1, metadata);

    assert!(!client.migrate_sbt_metadata(&sbt_id, &0));
    assert!(!client.migrate_sbt_metadata(&sbt_id, &1));
}

/// The v1 -> v2 migration prefixes a 1-byte schema tag onto the existing
/// metadata and bumps schema_version, leaving the rest of the bytes intact.
#[test]
fn test_migrate_v1_to_v2_prefixes_schema_tag() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);
    force_schema_version(&env, &client.address, sbt_id, 1, metadata.clone());
    assert_eq!(client.get_schema_version(&sbt_id), 1);

    assert!(client.migrate_sbt_metadata(&sbt_id, &2));

    assert_eq!(client.get_schema_version(&sbt_id), 2);
    let migrated = client.get_metadata(&sbt_id);
    let mut expected = Bytes::new(&env);
    expected.push_back(2u8);
    expected.append(&metadata);
    assert_eq!(migrated, expected);
}

/// A multi-step target (v1 -> CURRENT_SCHEMA_VERSION, currently 2) applies
/// each intermediate migration in sequence rather than jumping directly.
#[test]
fn test_migrate_applies_intermediate_steps_in_sequence() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0x01);
    let sbt_id = client.mint_sbt(&owner, &metadata);
    force_schema_version(&env, &client.address, sbt_id, 1, metadata);

    assert!(client.migrate_sbt_metadata(&sbt_id, &CURRENT_SCHEMA_VERSION));
    assert_eq!(client.get_schema_version(&sbt_id), CURRENT_SCHEMA_VERSION);
}

/// Only the SBT owner can migrate its metadata.
#[test]
#[should_panic]
fn test_migrate_requires_owner_auth() {
    let (env, _, owner, client) = setup();
    let metadata = bytes!(&env, 0xaabbcc);
    let sbt_id = client.mint_sbt(&owner, &metadata);
    force_schema_version(&env, &client.address, sbt_id, 1, metadata);

    env.set_auths(&[]);
    client.migrate_sbt_metadata(&sbt_id, &2);
}
