# Architecture Diagrams and Visual Guides

This document provides a complete visual reference for the Heirloom-Protocol system. All diagrams use [Mermaid](https://mermaid.js.org/) syntax and render natively in GitHub, GitLab, VS Code (with the Markdown Preview Mermaid Support extension), and most modern documentation platforms.

## Table of Contents

- [Diagram Conventions](#diagram-conventions)
- [Component Diagram](#component-diagram)
- [Data Flow Diagrams](#data-flow-diagrams)
  - [Check-In Flow](#check-in-flow)
  - [Vault Release Flow](#vault-release-flow)
  - [Withdrawal Flow](#withdrawal-flow)
  - [Deployment Flow](#deployment-flow)
- [Sequence Diagrams](#sequence-diagrams)
  - [Vault Creation and Funding](#vault-creation-and-funding)
  - [Owner Check-In](#owner-check-in)
  - [Beneficiary Release](#beneficiary-release)
  - [Passkey Registration and Biometric Check-In](#passkey-registration-and-biometric-check-in)
  - [Vault Hibernation](#vault-hibernation)
  - [TTL Borrowing](#ttl-borrowing)
  - [Beneficiary Conflict Resolution](#beneficiary-conflict-resolution)
  - [Vault Archival and Restoration](#vault-archival-and-restoration)
- [State Diagrams](#state-diagrams)
  - [Vault Lifecycle States](#vault-lifecycle-states)
  - [Passkey States](#passkey-states)
  - [Beneficiary Acceptance States](#beneficiary-acceptance-states)
- [Entity Relationship Diagram](#entity-relationship-diagram)
- [Technology Stack](#technology-stack)
- [Related Documentation](#related-documentation)

---

## Diagram Conventions

| Convention | Meaning |
|---|---|
| Solid arrows (`-->`) | Synchronous call or data flow |
| Dashed arrows (`-.->`) | Asynchronous call, event, or optional path |
| `[[ ]]` nodes | Database or persistent storage |
| `(( ))` nodes | External actor or system |
| `{ }` nodes | Decision point |
| Bold border nodes | Entry points (user-facing) |
| Green fill | Success / happy path |
| Red fill | Error / failure path |
| Orange fill | Warning / conditional path |

All diagrams show testnet/mainnet production flows. Local development (Docker Quickstart) follows the same logical flow with `localhost` endpoints replacing remote URLs.

---

## Component Diagram

The following diagram shows all major components and their communication channels.

```mermaid
graph TD
    subgraph Clients["Client Layer"]
        Mobile["📱 Mobile App\n(Android / iOS)"]
        Browser["🌐 Browser\n(Legacy Dashboard)"]
        Playground["🧪 Interactive\nPlayground"]
    end

    subgraph BackendLayer["Backend Layer"]
        API["⚙️ Backend API\n(Rust / Axum)"]
        Scheduler["🕐 Reminder\nScheduler"]
        WS["🔌 WebSocket\nServer"]
        Notif["🔔 Notification\nService"]
    end

    subgraph StorageLayer["Storage Layer"]
        DB[("🗄️ PostgreSQL\nDatabase")]
        Cache[("⚡ In-Memory\nCache")]
    end

    subgraph StellarLayer["Stellar / Soroban Layer"]
        Network["🌐 Stellar Network\n(testnet / mainnet)"]
        VaultContract["📦 ttl_vault\nContract"]
        ZKContract["🔐 zk_verifier\nContract"]
        SBTContract["🏷️ sbt\nContract"]
    end

    Mobile -->|"REST / WebSocket"| API
    Browser -->|"REST / WebSocket"| API
    Playground -->|"REST"| API

    API -->|"Stellar SDK"| Network
    Network -->|"Contract Invocation"| VaultContract
    Network -->|"ZK Proof Verification"| ZKContract
    Network -->|"SBT Issuance"| SBTContract

    API --> DB
    API --> Cache
    Scheduler -.->|"Trigger Reminder"| Notif
    Notif -.->|"Email / SMS"| Mobile
    WS -.->|"Real-time Events"| Browser
    WS -.->|"Real-time Events"| Mobile

    VaultContract -.->|"Contract Events"| API
```

---

## Data Flow Diagrams

### Check-In Flow

This flow shows how a vault owner proves liveness to extend TTL.

```mermaid
flowchart LR
    Owner(("👤 Owner"))
    Auth{"Passkey\nAuthenticated?"}
    Cooldown{"Within\nCooldown?"}
    PasskeyExpired{"Passkey\nExpired?"}
    Contract["ttl_vault\nContract"]
    UpdateTTL["Update\nlast_check_in"]
    EmitEvent["Emit\ncheck_in event"]
    ErrAuth["❌ Unauthorized"]
    ErrCooldown["❌ CheckInTooFrequent\n(error 54)"]
    ErrExpired["❌ PasskeyExpired\n(error 59)"]

    Owner -->|"check_in(vault_id)"| Auth
    Auth -->|"No"| ErrAuth
    Auth -->|"Yes"| Cooldown
    Cooldown -->|"Yes"| ErrCooldown
    Cooldown -->|"No"| PasskeyExpired
    PasskeyExpired -->|"Yes"| ErrExpired
    PasskeyExpired -->|"No"| Contract
    Contract --> UpdateTTL
    UpdateTTL --> EmitEvent
    EmitEvent -->|"✅ Success"| Owner
```

### Vault Release Flow

This flow shows how funds are transferred to the beneficiary when TTL expires.

```mermaid
flowchart TD
    Caller(("👤 Any Caller"))
    CheckExpired{"is_expired\n(vault_id)?"}
    CheckReleased{"Already\nReleased?"}
    CheckArchived{"Vault\nArchived?"}
    RestoreVault["restore_vault\n(auto)"]
    Transfer["Transfer Funds\nto Beneficiary"]
    EmitRelease["Emit release\nevent"]
    ErrNotExpired["❌ NotExpired"]
    ErrReleased["❌ AlreadyReleased"]

    Caller -->|"trigger_release(vault_id)"| CheckReleased
    CheckReleased -->|"Yes"| ErrReleased
    CheckReleased -->|"No"| CheckExpired
    CheckExpired -->|"No"| ErrNotExpired
    CheckExpired -->|"Yes"| CheckArchived
    CheckArchived -->|"Yes"| RestoreVault
    CheckArchived -->|"No"| Transfer
    RestoreVault --> Transfer
    Transfer --> EmitRelease
    EmitRelease -->|"✅ Funds Released"| Caller
```

### Withdrawal Flow

```mermaid
flowchart LR
    Owner(("👤 Owner"))
    AuthCheck{"Owner\nAuthenticated?"}
    ActiveCheck{"Vault\nActive?"}
    BalanceCheck{"Sufficient\nBalance?"}
    Withdraw["Execute\nWithdrawal"]
    AuditLog["Log to\nAudit Trail"]
    Notify["Send Withdrawal\nNotification"]
    DisputeWindow["24h Dispute\nWindow Opens"]
    ErrAuth["❌ Unauthorized"]
    ErrInactive["❌ Vault Expired\nor Released"]
    ErrBalance["❌ Insufficient\nBalance"]

    Owner -->|"withdraw(vault_id, amount)"| AuthCheck
    AuthCheck -->|"No"| ErrAuth
    AuthCheck -->|"Yes"| ActiveCheck
    ActiveCheck -->|"No"| ErrInactive
    ActiveCheck -->|"Yes"| BalanceCheck
    BalanceCheck -->|"No"| ErrBalance
    BalanceCheck -->|"Yes"| Withdraw
    Withdraw --> AuditLog
    Withdraw --> Notify
    Withdraw --> DisputeWindow
```

### Deployment Flow

```mermaid
flowchart TD
    Dev(("👤 Developer"))
    BuildWasm["cargo build\n--target wasm32-unknown-unknown"]
    OptimizeWasm["wasm-opt\n(binaryen)"]
    CheckBudget{"WASM within\nsize budget?"}
    UploadWasm["stellar contract\ninstall (upload WASM)"]
    DeployContract["stellar contract\ndeploy"]
    Initialize["invoke initialize\n(set admin, token)"]
    UpdateEnv["Update .env\nCONTRACT_TTL_VAULT"]
    Smoke["Smoke Test:\ncreate_vault + deposit"]
    ErrBudget["❌ Exceeds WASM\nsize budget"]

    Dev --> BuildWasm
    BuildWasm --> OptimizeWasm
    OptimizeWasm --> CheckBudget
    CheckBudget -->|"No"| ErrBudget
    CheckBudget -->|"Yes"| UploadWasm
    UploadWasm --> DeployContract
    DeployContract --> Initialize
    Initialize --> UpdateEnv
    UpdateEnv --> Smoke
    Smoke -->|"✅ Deployed"| Dev
```

---

## Sequence Diagrams

### Vault Creation and Funding

```mermaid
sequenceDiagram
    actor Owner
    participant App as Mobile / Browser
    participant Backend as Backend API
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract

    Owner->>App: Fill in beneficiary + interval
    App->>Backend: POST /api/vaults (beneficiary, interval)
    Backend->>Stellar: Sign + submit create_vault tx
    Stellar->>Vault: create_vault(beneficiary, check_in_interval)
    Vault-->>Stellar: vault_id = N
    Stellar-->>Backend: tx confirmed, vault_id
    Backend-->>App: { vault_id: N }
    App-->>Owner: "Vault #N created"

    Owner->>App: Enter deposit amount
    App->>Backend: POST /api/vaults/N/deposit (amount)
    Backend->>Stellar: Sign + submit deposit tx
    Stellar->>Vault: deposit(vault_id=N, amount)
    Vault-->>Stellar: balance updated
    Stellar-->>Backend: tx confirmed
    Backend-->>App: { balance: X }
    App-->>Owner: "Deposited X stroops"
```

### Owner Check-In

```mermaid
sequenceDiagram
    actor Owner
    participant App as Mobile / Browser
    participant Backend as Backend API
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract
    participant Notif as Notification Service

    Note over Notif,Owner: Reminder sent before TTL deadline
    Notif-->>Owner: "Check-in due in 24h" (email/SMS)

    Owner->>App: Tap "Check In"
    App->>App: Authenticate (Passkey / biometric)
    App->>Backend: POST /api/vaults/N/checkin (passkey_hash)
    Backend->>Stellar: Sign + submit check_in tx
    Stellar->>Vault: check_in(vault_id=N)
    Vault->>Vault: Validate passkey + cooldown
    Vault->>Vault: Update last_check_in = now
    Vault-->>Stellar: emit check_in event
    Stellar-->>Backend: tx confirmed
    Backend-->>App: { ttl_remaining: Y seconds }
    App-->>Owner: "Check-in successful. Next due in Y seconds."
```

### Beneficiary Release

```mermaid
sequenceDiagram
    actor Beneficiary
    participant App as Mobile / Browser
    participant Backend as Backend API
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract

    Note over Vault: TTL has lapsed (no check-in)

    Beneficiary->>App: Tap "Claim Vault #N"
    App->>Backend: POST /api/vaults/N/release
    Backend->>Stellar: Submit trigger_release tx
    Stellar->>Vault: trigger_release(vault_id=N)
    Vault->>Vault: Check is_expired() → true
    Vault->>Vault: Check AlreadyReleased → false
    alt Vault was archived
        Vault->>Vault: try_restore_archived_vault()
    end
    Vault->>Stellar: Transfer balance to beneficiary
    Vault-->>Stellar: emit release event
    Stellar-->>Backend: tx confirmed
    Backend-->>App: { status: "Released", amount: X }
    App-->>Beneficiary: "Funds received: X XLM"
```

### Passkey Registration and Biometric Check-In

```mermaid
sequenceDiagram
    actor Owner
    participant Device as User Device
    participant App as Mobile App
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract

    Note over Device: Raw biometric never leaves device

    Owner->>Device: Scan fingerprint / face
    Device->>Device: SHA-256 hash(biometric data)
    Device->>App: credential_hash (bytes32)
    App->>Stellar: bind_passkey_biometric(vault_id, passkey_hash, credential_hash)
    Stellar->>Vault: Store credential_hash on-chain
    Vault-->>Stellar: emit bio_reg event
    Stellar-->>App: confirmed
    App-->>Owner: "Biometric registered"

    Note over Owner: Later: performing a biometric check-in
    Owner->>Device: Scan fingerprint
    Device->>Device: SHA-256 hash(biometric data)
    Device->>App: credential_hash
    App->>Stellar: biometric_check_in(vault_id, passkey_hash, credential_hash)
    Stellar->>Vault: Verify credential_hash matches stored entry
    Vault->>Vault: Update last_check_in = now
    Vault-->>Stellar: emit bio_ci event
    Stellar-->>App: confirmed
    App-->>Owner: "Biometric check-in complete"
```

### Vault Hibernation

```mermaid
sequenceDiagram
    actor Owner
    participant App as Mobile / Browser
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract

    Owner->>App: "Enter hibernation for 90 days"
    App->>Stellar: enter_hibernation(vault_id, caller, 7776000)
    Stellar->>Vault: Store HibernationEntry
    Vault-->>Stellar: emit hibernation event
    Stellar-->>App: confirmed
    App-->>Owner: "Vault hibernating until [date]"

    Note over Vault: TTL countdown paused during hibernation

    Owner->>App: "Exit hibernation early"
    App->>Stellar: exit_hibernation(vault_id, caller)
    Stellar->>Vault: Remove HibernationEntry
    Vault->>Vault: Resume TTL countdown from now
    Vault-->>Stellar: emit exit_hibernation event
    Stellar-->>App: confirmed
    App-->>Owner: "Vault active again"
```

### TTL Borrowing

```mermaid
sequenceDiagram
    actor Owner
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract

    Note over Owner: Emergency — borrower vault TTL running low

    Owner->>Stellar: borrow_ttl(borrower_id, lender_id, caller, 86400)
    Stellar->>Vault: Validate both vaults owned by caller
    Vault->>Vault: Reduce lender last_check_in by 86400s
    Vault->>Vault: Extend borrower last_check_in by 86400s
    Vault->>Vault: Store TtlBorrowRecord on-chain
    Vault-->>Stellar: emit ttl_bor event
    Stellar-->>Owner: "TTL transferred: 1 day borrowed"

    Note over Owner: After emergency resolves

    Owner->>Stellar: repay_ttl_borrow(borrower_id, caller)
    Stellar->>Vault: Read TtlBorrowRecord
    Vault->>Vault: Restore lender last_check_in
    Vault->>Vault: Reduce borrower last_check_in
    Vault->>Vault: Delete TtlBorrowRecord
    Vault-->>Stellar: emit ttl_rep event
    Stellar-->>Owner: "TTL repaid"
```

### Beneficiary Conflict Resolution

```mermaid
sequenceDiagram
    actor ClaimantA as Claimant A
    actor ClaimantB as Claimant B
    participant Stellar as Stellar Network
    participant Vault as ttl_vault Contract
    participant Resolution as Conflict Resolution Logic

    Note over Vault: TTL expired

    ClaimantA->>Stellar: trigger_release(vault_id)
    ClaimantB->>Stellar: trigger_release(vault_id)

    Stellar->>Vault: Multiple claims detected
    Vault->>Resolution: Invoke conflict resolution
    Resolution->>Resolution: Calculate ranking scores
    Resolution->>Resolution: Apply beneficiary caps and floors
    Resolution->>Resolution: Determine winning claimant
    Resolution-->>Vault: Winner = Claimant A

    Vault->>Stellar: Transfer funds to Claimant A
    Vault-->>Stellar: emit release event (winner=A)
    Stellar-->>ClaimantA: "Funds received"
    Stellar-->>ClaimantB: "Claim rejected — Claimant A won resolution"
```

### Vault Archival and Restoration

```mermaid
sequenceDiagram
    participant Time as Soroban Network
    participant Vault as ttl_vault Contract
    participant OffChain as Off-chain Indexer
    participant Caller as Any Caller

    Note over Time: Owner inactive — no deposits, withdrawals, check-ins

    Time->>Vault: Persistent storage TTL reaches zero
    Vault->>Vault: State archived (not deleted)
    OffChain->>Vault: Detect archival; snapshot state
    OffChain->>Vault: Store ArchivedVaultInfo(vault_id)

    Note over Caller: Beneficiary wants to trigger release

    Caller->>Vault: trigger_release(vault_id)
    Vault->>Vault: try_restore_archived_vault()
    Vault->>Vault: Extend persistent entry TTL
    Vault->>Vault: Remove ArchivedVaultInfo snapshot
    Vault->>Vault: is_expired() → true
    Vault->>Stellar: Transfer funds to beneficiary
    Vault-->>Caller: emit release + v_restore events
```

---

## State Diagrams

### Vault Lifecycle States

```mermaid
stateDiagram-v2
    [*] --> Active : create_vault()

    Active --> Active : check_in()\n[TTL reset]
    Active --> Active : deposit() / withdraw()
    Active --> Hibernating : enter_hibernation()
    Active --> Expired : TTL lapses\n(no check-in)
    Active --> Archived : Soroban archives\ninactive state

    Hibernating --> Active : exit_hibernation()
    Hibernating --> Expired : Hibernation ends\n+ TTL already lapsed

    Archived --> Active : restore_vault()
    Archived --> Released : trigger_release()\n[auto-restores then releases]

    Expired --> Released : trigger_release()

    Released --> [*]

    note right of Active
        Owner can check in,
        deposit, and withdraw.
        TTL countdown is running.
    end note

    note right of Expired
        Anyone can call
        trigger_release.
        Owner check-in still
        possible to cancel.
    end note

    note right of Released
        Funds transferred to
        beneficiary. Terminal
        state — no further
        operations possible.
    end note
```

### Passkey States

```mermaid
stateDiagram-v2
    [*] --> Unregistered

    Unregistered --> Active : register_passkey() /\nbind_passkey_biometric()

    Active --> Expired : extend_passkey_expiry()\ndeadline reached
    Active --> Compromised : report_passkey_compromise()
    Active --> Unregistered : revoke_passkey()

    Expired --> Active : rotate passkey\n(revoke + re-register)

    Compromised --> Active : clear_passkey_compromise()
    Compromised --> Unregistered : revoke_passkey()

    note right of Active
        check_in accepts this
        passkey hash.
    end note

    note right of Expired
        check_in returns
        PasskeyExpired (59).
        pk_expd event emitted.
    end note

    note right of Compromised
        check_in returns
        PasskeyCompromised (62).
        pk_comp event emitted.
    end note
```

### Beneficiary Acceptance States

```mermaid
stateDiagram-v2
    [*] --> Nominated : create_vault() /\nupdate_beneficiary()

    Nominated --> PendingAcceptance : Vault balance\nexceeds threshold

    PendingAcceptance --> Accepted : accept_beneficiary_role()
    PendingAcceptance --> Rejected : reject_beneficiary_role()
    PendingAcceptance --> Escalated : Conflict detected\n(multiple claimants)

    Accepted --> Delegated : delegate_beneficiary_role()
    Accepted --> Paid : trigger_release()\n[vault expired]

    Delegated --> Paid : trigger_release()\n[funds go to delegate]

    Escalated --> Accepted : Conflict resolution\ncomplete (winner)
    Escalated --> Rejected : Conflict resolution\ncomplete (loser)

    Rejected --> [*]
    Paid --> [*]
```

---

## Entity Relationship Diagram

This diagram shows the key on-chain data entities and their relationships.

```mermaid
erDiagram
    VAULT {
        u64 vault_id PK
        Address owner
        Address beneficiary
        i128 balance
        u64 last_check_in
        u64 check_in_interval
        bool is_released
        bool is_paused
    }

    PASSKEY {
        u64 vault_id FK
        BytesN32 passkey_hash PK
        u64 expiry
        bool is_compromised
    }

    BIOMETRIC_ENTRY {
        u64 vault_id FK
        BytesN32 passkey_hash FK
        BytesN32 credential_hash PK
    }

    HIBERNATION_ENTRY {
        u64 vault_id PK_FK
        u64 start_time
        u64 duration_seconds
        Address initiated_by
    }

    TTL_BORROW_RECORD {
        u64 borrower_vault_id PK_FK
        u64 lender_vault_id FK
        u64 borrow_seconds
        u64 borrow_timestamp
    }

    GEO_CHECK_IN_ENTRY {
        u64 vault_id FK
        u64 timestamp
        i64 latitude_micro
        i64 longitude_micro
        String country_code
    }

    ARCHIVED_VAULT_INFO {
        u64 vault_id PK_FK
        u64 archived_at
        i128 balance_at_archive
    }

    VAULT ||--o{ PASSKEY : "has"
    PASSKEY ||--o{ BIOMETRIC_ENTRY : "has"
    VAULT ||--o| HIBERNATION_ENTRY : "may have"
    VAULT ||--o| TTL_BORROW_RECORD : "may have (as borrower)"
    VAULT ||--o{ GEO_CHECK_IN_ENTRY : "logs"
    VAULT ||--o| ARCHIVED_VAULT_INFO : "may have"
```

---

## Technology Stack

| Component | Technology | Rationale |
|---|---|---|
| **Smart Contracts** | Rust / Soroban | Secure, performant, and native to Stellar |
| **Backend API** | Rust / Axum | Type-safe, high-concurrency, efficient performance |
| **Database** | PostgreSQL | Reliable relational storage for off-chain backend state |
| **Mobile (Android)** | Kotlin / Jetpack Compose | Native performance and UI |
| **Mobile (iOS)** | Swift / SwiftUI | Native performance and UI |
| **Blockchain** | Stellar | Low cost, fast finality, robust smart contract platform |
| **Auth** | Passkeys / WebAuthn | Phishing-resistant, no seed phrase exposure |
| **ZK Proofs** | `zk_verifier` contract | On-chain passkey signature verification |
| **Soulbound Tokens** | `sbt` contract | Non-transferable identity and reputation tokens |
| **Notifications** | Email + SMS (configurable APIs) | Owner reminders for upcoming check-in deadlines |
| **Monitoring** | Prometheus-compatible metrics | Service health and contract event tracking |
| **CI** | GitHub Actions | Automated build, test, and lint on every PR |

---

## Related Documentation

- [docs/ttl-logic.md](ttl-logic.md) — detailed TTL and state archival mechanics
- [docs/passkeys.md](passkeys.md) — passkey and biometric authentication
- [docs/beneficiary-conflict-resolution.md](beneficiary-conflict-resolution.md) — conflict resolution algorithm
- [docs/hibernation.md](hibernation.md) — vault hibernation feature
- [docs/withdrawal-features.md](withdrawal-features.md) — withdrawal lifecycle and dispute
- [docs/security.md](security.md) — threat model and security properties
- [docs/zk-verifier.md](zk-verifier.md) — ZK verifier contract details
- [docs/sbt.md](sbt.md) — Soulbound Token contract details
- [docs/deployment-guide.md](deployment-guide.md) — end-to-end deployment guide
- [docs/faq.md](faq.md) — frequently asked questions
- [docs/playground.md](playground.md) — interactive playground for hands-on exploration
