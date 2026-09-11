#!/usr/bin/env bash
set -e

echo "Running Heirloom-Protocol tests..."
cargo test --manifest-path Heirloom-contracts/ttl_vault/Cargo.toml
echo "All tests passed."
