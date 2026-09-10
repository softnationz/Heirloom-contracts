#![cfg(test)]

use super::*;
use soroban_sdk::testutils::Address as _;

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let contract_id = env.register_contract(None, SbtContract);
    SbtContractClient::new(&env, &contract_id).initialize(&admin);

    (env, contract_id, owner)
}

#[test]
fn messagepack_compression_round_trips_repeated_metadata() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = Bytes::from_slice(&env, &[b'A'; 64]);

    let compressed = client.compress_metadata(&metadata);

    assert!(compressed.len() < metadata.len());
    assert_eq!(compressed.get(0).unwrap(), 0xC7);
    assert_eq!(client.decompress_metadata(&compressed), metadata);
}

#[test]
fn messagepack_ext16_round_trips_larger_compressed_metadata() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let mut metadata = Bytes::new(&env);
    for value in 0..134u32 {
        let byte = (value % 251) as u8;
        metadata.push_back(byte);
        metadata.push_back(byte);
        metadata.push_back(byte);
    }

    let compressed = client.compress_metadata(&metadata);

    assert!(compressed.len() < metadata.len());
    assert_eq!(compressed.get(0).unwrap(), 0xC8);
    assert_eq!(client.decompress_metadata(&compressed), metadata);
}

#[test]
fn compression_keeps_incompressible_metadata_unframed() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = Bytes::from_slice(&env, &[0, 17, 53, 102, 166, 245, 77, 190]);

    let compressed = client.compress_metadata(&metadata);

    assert_eq!(compressed, metadata);
    assert!(!compression::is_compressed(&compressed));
}

#[test]
fn decompression_is_backwards_compatible_with_raw_metadata() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = Bytes::from_slice(&env, b"existing uncompressed metadata");

    assert_eq!(client.decompress_metadata(&metadata), metadata);
}

#[test]
fn decompression_supports_legacy_delta_and_rle_metadata() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let legacy_delta = Bytes::from_slice(&env, &[0xC1, 1, b'A', 0, 0]);
    let legacy_rle = Bytes::from_slice(&env, &[0xC1, 0, 3, b'A']);
    let expected = Bytes::from_slice(&env, b"AAA");

    assert_eq!(client.decompress_metadata(&legacy_delta), expected);
    assert_eq!(client.decompress_metadata(&legacy_rle), expected);
}

#[test]
fn malformed_heirloom_messagepack_extension_is_rejected() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let malformed = Bytes::from_slice(&env, &[0xC7, 3, 0x45, 1, 0, 0]);

    assert!(client.try_decompress_metadata(&malformed).is_err());
}

#[test]
fn empty_metadata_round_trips_without_a_header() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = Bytes::new(&env);

    assert_eq!(client.compress_metadata(&metadata), metadata);
    assert_eq!(client.decompress_metadata(&metadata), metadata);
}

#[test]
fn in_place_compression_preserves_the_string_metadata_api() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = String::from_str(
        &env,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    );
    let credential_id = client.mint(&owner, &metadata);

    let bytes_saved = client.compress_sbt_metadata(&credential_id);

    assert_eq!(bytes_saved, 57);
    assert!(client.is_sbt_metadata_compressed(&credential_id));
    assert_eq!(client.get_metadata(&credential_id), metadata);
    assert_eq!(
        client.decompress_sbt_metadata(&credential_id),
        Bytes::from_slice(&env, &[b'A'; 64])
    );
    assert_eq!(client.compress_sbt_metadata(&credential_id), 0);
}

#[test]
fn in_place_compression_leaves_larger_encodings_unmodified() {
    let (env, contract_id, owner) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = String::from_str(&env, "short");
    let credential_id = client.mint(&owner, &metadata);

    assert_eq!(client.compress_sbt_metadata(&credential_id), 0);
    assert!(!client.is_sbt_metadata_compressed(&credential_id));
    assert_eq!(client.get_metadata(&credential_id), metadata);
}

#[test]
fn benchmark_repeated_metadata_storage_savings() {
    let (env, contract_id, _) = setup();
    let client = SbtContractClient::new(&env, &contract_id);
    let metadata = Bytes::from_slice(&env, &[b'A'; 64]);
    let compressed = client.compress_metadata(&metadata);
    let bytes_saved = metadata.len() - compressed.len();
    let savings_bps = bytes_saved * 10_000 / metadata.len();

    assert_eq!(metadata.len(), 64);
    assert_eq!(compressed.len(), 7);
    assert_eq!(bytes_saved, 57);
    assert_eq!(savings_bps, 8_906);
}
