# WebAuthn / FIDO2 Setup Guide (#148)

Heirloom-Protocol supports WebAuthn (FIDO2) for phishing-resistant, hardware-backed
authentication.  Vault owners can register security keys (YubiKey, Apple Touch ID,
Windows Hello, etc.) and use them to perform check-ins without a seed phrase.

---

## Overview

WebAuthn is a W3C standard that lets servers authenticate users via public-key
cryptography instead of passwords.  The private key never leaves the authenticator
device.

Supported authenticator types:

| Category | Examples |
|---|---|
| Hardware security keys | YubiKey 5, Titan Key, SoloKey |
| Platform authenticators | Apple Touch ID / Face ID, Windows Hello |
| Mobile | Android Fingerprint, iPhone Passkeys |

---

## Architecture

```
Client                Backend                   Authenticator
  │                      │                           │
  │─ POST /webauthn/ ────►│                           │
  │  register/begin       │ generate challenge        │
  │◄─ challenge ──────────│                           │
  │                       │                           │
  │─────────── navigator.credentials.create() ───────►│
  │◄─────────── credential (attestation) ─────────────│
  │                       │                           │
  │─ POST /webauthn/ ────►│                           │
  │  register/complete    │ verify + store            │
  │◄─ 201 Created ────────│                           │
```

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `WEBAUTHN_RP_ID` | `localhost` | Relying Party ID — must match the domain serving the frontend |
| `WEBAUTHN_RP_NAME` | `Heirloom Protocol` | Human-readable name shown by authenticators |
| `WEBAUTHN_ORIGIN` | `http://localhost:3000` | Exact origin of your frontend (`scheme://host[:port]`) |

Set these before starting the backend:

```bash
export WEBAUTHN_RP_ID=app.ethos-protocol.xyz
export WEBAUTHN_RP_NAME="Heirloom Protocol"
export WEBAUTHN_ORIGIN=https://app.ethos-protocol.xyz
```

---

## API Reference

### Registration

#### `POST /webauthn/register/begin`

Start a registration ceremony.  Returns a challenge the browser must sign.

**Request**

```json
{
  "user_id": "user-uuid",
  "user_name": "alice@example.com",
  "label": "YubiKey 5C",
  "is_backup": false
}
```

**Response `200 OK`**

```json
{
  "session_id": "<token>",
  "rp": { "id": "example.com", "name": "Heirloom Protocol" },
  "user": { "id": "<b64url-user-handle>", "name": "alice@example.com", "display_name": "alice@example.com" },
  "challenge": "<b64url-challenge>",
  "pub_key_cred_params": [
    { "type": "public-key", "alg": -7 },
    { "type": "public-key", "alg": -8 },
    { "type": "public-key", "alg": -257 }
  ],
  "timeout_ms": 300000,
  "attestation": "none"
}
```

Pass the returned fields directly to `navigator.credentials.create()`.

---

#### `POST /webauthn/register/complete`

Complete registration after the browser returns the authenticator response.

**Request**

```json
{
  "session_id": "<token from begin>",
  "credential_id": "<b64url-credential-id>",
  "client_data_json": "<b64url-client-data-json>",
  "attestation_object": "<b64url-attestation-object>",
  "label": "YubiKey 5C",
  "is_backup": false
}
```

**Response `201 Created`**

```json
{
  "credential_id": "...",
  "user_id": "user-uuid",
  "is_backup": false,
  "label": "YubiKey 5C",
  "created_at": "2026-07-29T14:00:00Z"
}
```

---

### Authentication

#### `POST /webauthn/authenticate/begin`

Start an authentication ceremony.

**Request**

```json
{ "user_id": "user-uuid" }
```

**Response `200 OK`**

```json
{
  "session_id": "<token>",
  "challenge": "<b64url-challenge>",
  "timeout_ms": 300000,
  "rp_id": "example.com",
  "allow_credentials": [
    { "type": "public-key", "id": "<b64url-cred-id>" }
  ],
  "user_verification": "preferred"
}
```

---

#### `POST /webauthn/authenticate/complete`

Complete authentication after the browser returns the assertion.

**Request**

```json
{
  "session_id": "<token from begin>",
  "credential_id": "<b64url-credential-id>",
  "client_data_json": "<b64url-client-data-json>",
  "authenticator_data": "<b64url-auth-data>",
  "signature": "<b64url-signature>"
}
```

**Response `200 OK`**

```json
{
  "user_id": "user-uuid",
  "credential_id": "...",
  "sign_count": 42,
  "authenticated_at": "2026-07-29T14:01:00Z"
}
```

---

### Credential Management

#### `GET /webauthn/credentials/:user_id`

List all registered credentials for a user.

**Response `200 OK`** — array of credential objects:

```json
[
  {
    "credential_id": "...",
    "user_id": "user-uuid",
    "algorithm": "ES256",
    "sign_count": 42,
    "is_backup": false,
    "label": "YubiKey 5C",
    "aaguid": null,
    "created_at": "2026-07-29T12:00:00Z",
    "last_used_at": "2026-07-29T14:01:00Z"
  }
]
```

---

#### `DELETE /webauthn/credentials/:user_id/:cred_id`

Remove a registered credential.  Returns `204 No Content` on success.

---

### Backup Authenticators

#### `POST /webauthn/credentials/:user_id/:cred_id/backup`

Mark an existing credential as a backup authenticator, or begin a new
backup-flagged registration challenge.

**Request**

```json
{
  "user_id": "user-uuid",
  "label": "Backup YubiKey"
}
```

**Response `200 OK`** (credential already registered — flag flipped):

```json
{
  "credential_id": "...",
  "is_backup": true,
  "message": "credential marked as backup authenticator"
}
```

**Response `202 Accepted`** (new credential — complete registration):

```json
{
  "session_id": "...",
  "challenge": "...",
  "is_backup": true,
  "message": "complete registration with is_backup=true to add backup authenticator",
  "pub_key_cred_params": [...],
  "timeout_ms": 300000
}
```

---

## JavaScript Integration Example

```js
// Registration
const beginResp = await fetch('/webauthn/register/begin', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ user_id: userId, user_name: email }),
});
const options = await beginResp.json();

// Decode base64url fields for the browser API
options.challenge = base64urlDecode(options.challenge);
options.user.id = base64urlDecode(options.user.id);

const credential = await navigator.credentials.create({ publicKey: options });

// Send attestation to server
await fetch('/webauthn/register/complete', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({
    session_id: options.session_id,
    credential_id: base64urlEncode(credential.rawId),
    client_data_json: base64urlEncode(credential.response.clientDataJSON),
    attestation_object: base64urlEncode(credential.response.attestationObject),
    label: 'My Security Key',
  }),
});
```

---

## Security Properties

- **Challenges are single-use** and expire after 5 minutes.
- **Origin binding**: the server validates that `origin` in `clientDataJSON` matches `WEBAUTHN_ORIGIN`.
- **Assertion signature verification**: during registration, the server parses the COSE
  public key out of the attestation object's `authData` — rejecting registration if the
  key can't be parsed — and stores it alongside the credential's declared algorithm
  (`ES256`, `RS256`, or `EdDSA`). During authentication, the server reconstructs
  `authenticatorData || SHA-256(clientDataJSON)` and cryptographically verifies
  `signature` against that stored public key. Authentication fails with `401 Unauthorized`
  if the signature is invalid. This is what actually proves possession of the
  authenticator's private key — earlier versions of this endpoint only checked that a
  signature was present and non-empty, without verifying it cryptographically.
- **Authenticator cloning detection**: *after* signature verification succeeds, the server
  additionally rejects authentication with `403 Forbidden` if the sign counter does not
  increase (indicates a cloned authenticator). This is a secondary, defense-in-depth
  check — not a substitute for signature verification.
- **Duplicate credential prevention**: the same credential ID cannot be registered twice.
- **Backup credentials**: up to N backup authenticators per user, each flagged `is_backup=true`
  for differentiated UX treatment.

> **Not yet implemented**: attestation *statement* verification (certificate chain
> validation for formats like `"packed"` or `"tpm"`) is not performed. The server requests
> `attestation: "none"`, so only the credential's own public key is cryptographically
> verified — not the authenticator manufacturer's attestation certificate.
