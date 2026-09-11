#!/usr/bin/env bash
set -e

# Load environment variables from .env if it exists
if [ -f .env ]; then
  # Use grep/xargs to avoid exporting comments or malformed lines
  export $(grep -v '^#' .env | xargs)
fi

# Audit required environment variables
# Note: build.sh itself doesn't strictly need these for compilation, but we check them
# because they are essential for the overall Heirloom-Protocol setup as per README.
REQUIRED_VARS=("STELLAR_NETWORK" "STELLAR_RPC_URL" "REMINDER_EMAIL_API_KEY" "REMINDER_SMS_API_KEY")

for var in "${REQUIRED_VARS[@]}"; do
  if [ -z "${!var}" ]; then
    echo "Warning: Required environment variable '$var' is not set. Check your .env file."
  fi
done

echo "Building Heirloom-Protocol contracts..."
cargo build --target wasm32-unknown-unknown --release --manifest-path Heirloom-contracts/ttl_vault/Cargo.toml
cargo build --target wasm32-unknown-unknown --release --manifest-path Heirloom-contracts/zk_verifier/Cargo.toml
cargo build --target wasm32-unknown-unknown --release --manifest-path Heirloom-contracts/sbt/Cargo.toml
echo "Build complete."
