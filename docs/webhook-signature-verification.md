# Webhook Signature Verification (#149)

All outgoing webhook deliveries from Heirloom-Protocol are signed with an HMAC
digest.  Consumers must verify the signature before processing a payload to
ensure it is authentic and has not been tampered with in transit.

---

## How It Works

When a webhook endpoint is registered with a `secret`, each delivery includes
two additional headers:

| Header | Example value | Purpose |
|---|---|---|
| `X-Heirloom-Signature` | `sha256=3e4a9c...` | HMAC digest of the raw request body |
| `X-Heirloom-Timestamp` | `1753704000` | Unix timestamp (seconds) of delivery |
| `X-Heirloom-Delivery` | `uuid-v4` | Unique delivery identifier |

The signature format is `<algorithm>=<hex-digest>`, where `<algorithm>` is one
of `sha256` (default), `sha1`, or `sha512`.

Consumers must:

1. Validate that `X-Heirloom-Timestamp` is within **±5 minutes** of the current time
   to prevent replay attacks.
2. Compute the expected HMAC digest from the raw request body and the shared secret.
3. Compare the computed digest against `X-Heirloom-Signature` using a **constant-time**
   comparison to prevent timing attacks.

---

## Supported Algorithms

| Algorithm | Header prefix | Notes |
|---|---|---|
| HMAC-SHA256 | `sha256` | Default — recommended for new integrations |
| HMAC-SHA1 | `sha1` | Legacy compatibility only |
| HMAC-SHA512 | `sha512` | Highest security margin |

Choose the algorithm when registering a webhook:

```json
POST /webhooks
{
  "url": "https://your-endpoint.example.com/webhooks",
  "secret": "your-shared-secret",
  "algorithm": "sha256"
}
```

---

## Verification Endpoint

Consumers who prefer server-side verification can call:

### `POST /webhooks/verify`

**Request**

```json
{
  "body": "<raw payload body as a string>",
  "secret": "your-shared-secret",
  "signature": "sha256=3e4a9c...",
  "timestamp": "1753704000"
}
```

**Response `200 OK`** (valid):

```json
{
  "valid": true,
  "algorithm": "sha256",
  "reason": null
}
```

**Response `401 Unauthorized`** (invalid):

```json
{
  "valid": false,
  "algorithm": "sha256",
  "reason": "signature mismatch"
}
```

Possible `reason` values:

| Reason | Meaning |
|---|---|
| `missing X-Heirloom-Signature header` | Signature header absent |
| `missing X-Heirloom-Timestamp header` | Timestamp header absent |
| `malformed signature header (expected '<alg>=<hex>')` | Header format wrong |
| `unsupported algorithm: <alg>` | Unknown algorithm prefix |
| `timestamp out of tolerance: ...` | Replay window exceeded |
| `signature mismatch` | HMAC digest does not match |

---

## Client-Side Verification Examples

### Node.js

```js
const crypto = require('crypto');

function verifyWebhook(rawBody, secret, signatureHeader, timestampHeader) {
  // 1. Validate timestamp
  const now = Math.floor(Date.now() / 1000);
  const ts = parseInt(timestampHeader, 10);
  if (Math.abs(now - ts) > 300) {
    throw new Error('Timestamp out of tolerance — possible replay attack');
  }

  // 2. Parse algorithm and received digest
  const [alg, received] = signatureHeader.split('=');
  const hmacAlg = alg === 'sha512' ? 'sha512' : alg === 'sha1' ? 'sha1' : 'sha256';

  // 3. Compute expected digest
  const expected = crypto
    .createHmac(hmacAlg, secret)
    .update(rawBody)
    .digest('hex');

  // 4. Constant-time compare
  const a = Buffer.from(received, 'hex');
  const b = Buffer.from(expected, 'hex');
  if (a.length !== b.length || !crypto.timingSafeEqual(a, b)) {
    throw new Error('Invalid signature');
  }
}

// Express middleware example
app.post('/webhooks', (req, res) => {
  verifyWebhook(
    req.rawBody,                         // must be the raw body string/buffer
    process.env.WEBHOOK_SECRET,
    req.headers['x-heirloom-signature'],
    req.headers['x-heirloom-timestamp'],
  );
  // Process event...
  res.sendStatus(200);
});
```

### Python

```python
import hashlib
import hmac
import time

def verify_webhook(raw_body: bytes, secret: str,
                   signature_header: str, timestamp_header: str) -> None:
    # 1. Validate timestamp
    ts = int(timestamp_header)
    if abs(time.time() - ts) > 300:
        raise ValueError("Timestamp out of tolerance")

    # 2. Parse algorithm
    alg, received = signature_header.split("=", 1)
    digestmod = {"sha256": hashlib.sha256, "sha1": hashlib.sha1,
                 "sha512": hashlib.sha512}[alg]

    # 3. Compute expected digest
    expected = hmac.new(secret.encode(), raw_body, digestmod).hexdigest()

    # 4. Constant-time compare
    if not hmac.compare_digest(received, expected):
        raise ValueError("Invalid signature")
```

### Go

```go
package webhook

import (
    "crypto/hmac"
    "crypto/sha256"
    "encoding/hex"
    "fmt"
    "math"
    "strconv"
    "strings"
    "time"
)

func Verify(body []byte, secret, signatureHeader, timestampHeader string) error {
    // 1. Validate timestamp
    ts, err := strconv.ParseInt(timestampHeader, 10, 64)
    if err != nil || math.Abs(float64(time.Now().Unix()-ts)) > 300 {
        return fmt.Errorf("timestamp out of tolerance")
    }

    // 2. Parse signature
    parts := strings.SplitN(signatureHeader, "=", 2)
    if len(parts) != 2 {
        return fmt.Errorf("malformed signature header")
    }
    received, _ := hex.DecodeString(parts[1])

    // 3. Compute expected (sha256 shown; swap hash func for sha1/sha512)
    mac := hmac.New(sha256.New, []byte(secret))
    mac.Write(body)
    expected := mac.Sum(nil)

    // 4. Constant-time compare
    if !hmac.Equal(received, expected) {
        return fmt.Errorf("signature mismatch")
    }
    return nil
}
```

---

## Security Notes

- **Never log or expose your webhook secret.**  Rotate it immediately if compromised.
- **Always use the raw request body** for HMAC computation.  Parsing the JSON and
  re-serialising it may change whitespace or key ordering, producing a different digest.
- **Reject requests outside the 5-minute timestamp window** to prevent replay attacks
  even if an attacker intercepts a valid delivery.
- **Prefer SHA-256** for new integrations.  SHA-1 is provided only for legacy
  compatibility and should be avoided where possible.

---

## Replay Attack Protection

The 5-minute timestamp tolerance (`TIMESTAMP_TOLERANCE_SECS = 300`) means that
even if an attacker captures a valid signed request, they cannot replay it
successfully more than 5 minutes after the original delivery timestamp.

For extra protection, consumers can persist the `X-Heirloom-Delivery` UUID and
reject duplicate delivery IDs within a retention window.

---

## Algorithm Selection by Registration

When registering a webhook, specify the algorithm you want the server to use:

```bash
curl -X POST http://localhost:3000/webhooks \
  -H 'Content-Type: application/json' \
  -d '{
    "url": "https://example.com/hooks",
    "secret": "my-shared-secret",
    "algorithm": "sha512",
    "event_types": ["vault_released"]
  }'
```

The server will then send `X-Heirloom-Signature: sha512=<hex>` on all deliveries
to that endpoint.
