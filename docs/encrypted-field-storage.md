# Encrypted Field Storage (#101)

Heirloom-Protocol encrypts sensitive database fields at rest using AES-256-GCM
authenticated encryption.  Each encrypted value carries its key version so
that multiple key versions can coexist during a rotation grace period.

## Sensitive fields

The following fields are encrypted before storage:

| Table                   | Column     | Reason                              |
|-------------------------|------------|-------------------------------------|
| `two_factor_config`     | `secret`   | TOTP seed — highly sensitive        |
| `two_factor_config`     | `phone`    | PII — phone number                  |
| `two_factor_config`     | `email`    | PII — email address                 |
| `reminder_preferences`  | `channels` | Contact details (email/phone/push)  |
| `unsubscribe_tokens`    | `owner`    | Owner identifier (email/phone)      |

## Wire format

Encrypted values are stored as a JSON object:

```json
{
  "ciphertext": "<base64(ciphertext || 16-byte tag)>",
  "nonce":      "<base64(12-byte random nonce)>",
  "key_version": 1
}
```

The ciphertext blob includes a 16-byte HMAC-SHA256-based authentication tag
appended after the ciphertext bytes.  Decryption verifies the tag first using
a constant-time comparison before returning plaintext.

## Key management

Keys are loaded from environment variables:

```
FIELD_ENCRYPTION_KEY_VERSION=1
FIELD_ENCRYPTION_KEY_1=<base64-encoded 32-byte key>
```

Generate a key:

```bash
openssl rand -base64 32
```

### Key rotation

1. Generate a new key and set `FIELD_ENCRYPTION_KEY_2=<new key>`.
2. Set `FIELD_ENCRYPTION_KEY_VERSION=2`.
3. Keep `FIELD_ENCRYPTION_KEY_1` in the environment during the grace period
   so that existing ciphertexts (version 1) can still be decrypted.
4. Migrate stored ciphertexts with a background job using
   `FieldEncryptionEngine::rotate_field`.
5. Once all records are migrated, record the key retirement via
   `GET /api/encryption/keys` and remove `FIELD_ENCRYPTION_KEY_1`.

Key version metadata is tracked in the `encryption_key_versions` table.

## API

```
GET /api/encryption/keys           — list all key versions and statuses
```

Response:

```json
[
  { "version": 1, "status": "retiring", "created_at": "...", "rotated_at": "..." },
  { "version": 2, "status": "active",   "created_at": "...", "rotated_at": null  }
]
```

## Implementation

The engine lives in `backend/src/encryption.rs`.  Key helpers:

```rust
let engine = FieldEncryptionEngine::from_env()?;

// Encrypt a value:
let field: EncryptedField = engine.encrypt("user@example.com")?;

// Decrypt:
let plaintext: String = engine.decrypt(&field)?;

// Rotate a single field to the new key version:
let (new_field, result) = engine.rotate_field(&field, 2)?;
```

## Development / test mode

If `FIELD_ENCRYPTION_KEY_<N>` is not set in the environment the engine falls
back to a zero-key for that version.  A `tracing::warn` is emitted.  **Never
deploy without setting real keys.**
