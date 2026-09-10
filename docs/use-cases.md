# Use Case Documentation

This document describes common use cases for Heirloom-Protocol, providing step-by-step instructions, code examples, troubleshooting guidance, and variations for each scenario.

## Table of Contents

- [UC-01: Personal Inheritance Vault](#uc-01-personal-inheritance-vault)
- [UC-02: Time-Locked Savings Vault](#uc-02-time-locked-savings-vault)
- [UC-03: Dead Man's Switch for Sensitive Data](#uc-03-dead-mans-switch-for-sensitive-data)
- [UC-04: Family Emergency Fund](#uc-04-family-emergency-fund)
- [UC-05: Conditional Beneficiary Acceptance](#uc-05-conditional-beneficiary-acceptance)
- [UC-06: Multi-Vault Portfolio Management](#uc-06-multi-vault-portfolio-management)
- [UC-07: Vault Hibernation During Inactivity](#uc-07-vault-hibernation-during-inactivity)
- [UC-08: Beneficiary Conflict Resolution](#uc-08-beneficiary-conflict-resolution)
- [UC-09: Vault with Vesting Schedule](#uc-09-vault-with-vesting-schedule)
- [UC-10: Emergency TTL Borrowing](#uc-10-emergency-ttl-borrowing)

---

## UC-01: Personal Inheritance Vault

### Description

An individual holds XLM and wants to ensure a family member receives the funds automatically if they become incapacitated or pass away without being able to manually transfer assets.

### Actors

- **Owner**: The XLM holder setting up the inheritance plan
- **Beneficiary**: The family member designated to receive funds

### Prerequisites

- Owner has a funded Stellar account
- Passkey (WebAuthn) registered for the owner account
- Beneficiary has a valid Stellar address

### Step-by-Step Instructions

**1. Deploy and configure the vault**

```bash
# Deploy the ttl_vault contract to testnet
./scripts/deploy_testnet.sh

# Note your CONTRACT_TTL_VAULT address from the output
```

**2. Create the vault with a beneficiary and check-in interval**

```rust
// Example: 90-day check-in interval (in seconds)
let check_in_interval: u64 = 90 * 24 * 60 * 60; // 7,776,000 seconds

let vault_id = contract.create_vault(
    beneficiary_address,
    check_in_interval,
);
```

**3. Deposit funds into the vault**

```rust
contract.deposit(vault_id, 1_000_0000000_i128); // 1000 XLM (in stroops)
```

**4. Set up regular check-ins**

```rust
// Owner checks in every 30 days to extend the TTL
contract.check_in(vault_id);
```

**5. Configure reminder notifications**

```bash
# In .env
REMINDER_EMAIL_API_KEY=<your-key>
REMINDER_SMS_API_KEY=<your-key>
```

The backend scheduler will send reminders before the check-in deadline.

**6. Release (triggered automatically when TTL expires)**

```rust
// After TTL lapses, anyone can trigger the release
contract.trigger_release(vault_id);
```

### Expected Outcomes

- Vault is created with the specified check-in interval
- Funds remain locked while the owner checks in regularly
- When the owner fails to check in, TTL expires and `trigger_release` transfers funds to the beneficiary

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| `NotExpired` error on `trigger_release` | TTL has not elapsed yet | Wait for check-in interval to pass |
| Passkey authentication fails | Passkey not registered or device changed | Re-register passkey via the dashboard |
| Vault not found | Wrong `vault_id` | Query `get_vault(vault_id)` to verify |
| Reminder notifications not sent | Missing API keys | Set `REMINDER_EMAIL_API_KEY` and `REMINDER_SMS_API_KEY` in `.env` |

### Variations

- **Shorter check-in interval (30 days)**: Provides faster release but requires more frequent check-ins
- **Longer check-in interval (1 year)**: Suitable for low-risk long-term planning
- **Multiple beneficiary splits**: See [Beneficiary Advanced Features](beneficiary-advanced-features.md)

---

## UC-02: Time-Locked Savings Vault

### Description

A user deposits XLM into a vault and sets themselves as the beneficiary, using the TTL mechanism as a forced savings lock to prevent early withdrawal.

### Actors

- **Owner/Beneficiary**: Same individual using the vault as a savings mechanism

### Prerequisites

- Funded Stellar account
- Understanding that funds are inaccessible until the vault expires (or the owner withdraws via standard withdrawal)

### Step-by-Step Instructions

**1. Create vault with yourself as beneficiary**

```rust
let vault_id = contract.create_vault(
    owner_address, // beneficiary = owner
    365 * 24 * 60 * 60, // 1-year lock
);
```

**2. Deposit funds**

```rust
contract.deposit(vault_id, 5_000_0000000_i128); // 5000 XLM
```

**3. Stop checking in when you want to unlock**

Once the check-in interval passes without a check-in, call:

```rust
contract.trigger_release(vault_id);
// Funds transfer to beneficiary (yourself)
```

**4. Or withdraw directly as owner**

```rust
// Owners can always withdraw their own funds before TTL expires
contract.withdraw(vault_id, amount);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Accidental check-in resets the timer | Automated check-in configured | Disable the reminder/scheduler integration |
| Funds released too early | `check_in_interval` set too short | Set a longer interval at vault creation |

### Variations

- **Recurring deposits**: Use `deposit()` repeatedly to add to the savings pool
- **Partial withdrawal**: Use `withdraw()` for emergency partial access

---

## UC-03: Dead Man's Switch for Sensitive Data

### Description

A user holds a vault whose release triggers an on-chain event that a separate off-chain system monitors to reveal encrypted credentials, documents, or messages.

### Actors

- **Owner**: Operator of the dead man's switch
- **Beneficiary**: Recipient of the off-chain notification or data

### Prerequisites

- Off-chain system (e.g., backend webhook) monitoring vault release events
- Encrypted data stored off-chain, keyed to vault release

### Step-by-Step Instructions

**1. Create a vault with a short check-in interval**

```rust
let vault_id = contract.create_vault(
    beneficiary_address,
    7 * 24 * 60 * 60, // 7-day switch
);
```

**2. Register a webhook for release events**

```bash
# POST to backend API
curl -X POST http://localhost:3000/api/webhooks \
  -H "Content-Type: application/json" \
  -d '{"vault_id": 1, "event": "trigger_release", "url": "https://your-server/release-hook"}'
```

**3. Upload encrypted payload linked to vault**

Store your encrypted document/message off-chain, associated with the `vault_id`. When the webhook fires, your system decrypts and delivers it to the beneficiary.

**4. Check in regularly to keep the switch armed**

```rust
contract.check_in(vault_id);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Webhook not firing | Backend not running or URL unreachable | Verify backend service and webhook endpoint |
| Double-trigger | `trigger_release` called multiple times | Check `get_release_status(vault_id)` before triggering |

### Variations

- **Longer interval (30 days)**: Less frequent check-ins, lower operational overhead
- **Geographic check-in**: Use `check_in_with_geo()` to add anomaly detection

---

## UC-04: Family Emergency Fund

### Description

Parents set up a vault with their children as beneficiaries, maintaining it as a family emergency fund that releases automatically upon both parents becoming incapacitated.

### Actors

- **Owner**: Parent(s) managing the vault
- **Beneficiary**: Children or designated family member

### Prerequisites

- All parties have Stellar addresses
- Owner has Passkey registered

### Step-by-Step Instructions

**1. Create the vault**

```rust
let vault_id = contract.create_vault(
    primary_beneficiary,
    180 * 24 * 60 * 60, // 6-month interval
);
```

**2. Set a beneficiary minimum threshold**

Ensure the vault only releases when the balance is meaningful:

```rust
// See beneficiary-minimum-threshold.md for full API
contract.set_beneficiary_minimum_threshold(vault_id, 500_0000000_i128); // 500 XLM
```

**3. Add funds periodically**

```rust
contract.deposit(vault_id, amount);
```

**4. Beneficiary accepts the role**

```rust
// Beneficiary confirms acceptance once threshold is met
contract.accept_beneficiary_role(vault_id);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Beneficiary cannot accept | Threshold not met | Deposit more funds above the threshold |
| Release not triggering | Owner still checking in | Review check-in schedule |

### Variations

- **Conditional acceptance**: See [UC-05](#uc-05-conditional-beneficiary-acceptance)
- **Multiple beneficiaries**: See [Beneficiary Advanced Features](beneficiary-advanced-features.md)

---

## UC-05: Conditional Beneficiary Acceptance

### Description

A beneficiary only accepts the role if the vault balance meets a minimum threshold, protecting them from inheriting a vault with negligible funds.

### Actors

- **Owner**: Vault creator and fund depositor
- **Beneficiary**: Conditional acceptor

### Prerequisites

- Vault created with a threshold configured
- Beneficiary has a valid Stellar address

### Step-by-Step Instructions

**1. Create vault with threshold**

```rust
let vault_id = contract.create_vault(beneficiary_address, check_in_interval);
contract.set_beneficiary_minimum_threshold(vault_id, 100_0000000_i128); // 100 XLM min
```

**2. Deposit sufficient funds**

```rust
contract.deposit(vault_id, 200_0000000_i128); // above threshold
```

**3. Beneficiary checks the threshold**

```rust
let threshold = contract.get_beneficiary_minimum_threshold(vault_id);
let vault = contract.get_vault(vault_id);
// Verify vault.balance >= threshold
```

**4. Beneficiary accepts**

```rust
contract.accept_beneficiary_role(vault_id);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| `ThresholdNotMet` error | Balance below minimum | Owner must deposit more funds |
| Acceptance window expired | Too much time passed | Owner resets acceptance window |

### Variations

- **No threshold**: Omit `set_beneficiary_minimum_threshold` for unconditional acceptance
- **Dynamic threshold**: Owner can update threshold before beneficiary accepts

For full API reference, see [Beneficiary Conditional Acceptance](beneficiary-conditional-acceptance.md).

---

## UC-06: Multi-Vault Portfolio Management

### Description

A user manages multiple vaults for different purposes — one for each family member, one for a business partner, one for a charitable donation.

### Actors

- **Owner**: Manages all vaults
- **Multiple beneficiaries**: One per vault

### Step-by-Step Instructions

**1. Create multiple vaults**

```rust
let family_vault = contract.create_vault(spouse_address, 90_day_interval);
let business_vault = contract.create_vault(partner_address, 180_day_interval);
let charity_vault = contract.create_vault(charity_address, 365_day_interval);
```

**2. Deposit into each vault**

```rust
contract.deposit(family_vault, 10_000_0000000_i128);
contract.deposit(business_vault, 5_000_0000000_i128);
contract.deposit(charity_vault, 1_000_0000000_i128);
```

**3. Check in on each vault regularly**

```rust
// Check in on all vaults in one session
for vault_id in [family_vault, business_vault, charity_vault] {
    contract.check_in(vault_id);
}
```

**4. Monitor TTL remaining**

```rust
let ttl_family = contract.get_ttl_remaining(family_vault);
let ttl_business = contract.get_ttl_remaining(business_vault);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Missed check-in on one vault | Too many vaults to track | Use the reminder system for each vault |
| Wrong vault released | Incorrect `vault_id` | Always query `get_vault(vault_id)` to confirm |

### Variations

- **TTL Borrowing**: If you miss a check-in on one vault, borrow TTL from another — see [UC-10](#uc-10-emergency-ttl-borrowing)

---

## UC-07: Vault Hibernation During Inactivity

### Description

An owner plans to travel or be unreachable for an extended period and puts their vault into hibernation to pause the TTL countdown.

### Actors

- **Owner**: Initiates and exits hibernation

### Step-by-Step Instructions

**1. Enter hibernation before departure**

```rust
let duration_seconds: u64 = 30 * 24 * 60 * 60; // 30-day trip
contract.enter_hibernation(vault_id, owner_address, duration_seconds);
```

**2. TTL countdown is paused**

While in hibernation, `is_expired()` returns `false` regardless of elapsed time.

**3. Exit hibernation on return**

```rust
contract.exit_hibernation(vault_id, owner_address);
```

**4. Verify hibernation status**

```rust
let hibernation = contract.get_hibernation(vault_id);
// Some(HibernationEntry) if active, None if not
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Cannot exit hibernation | Not the owner | Only the original owner can call `exit_hibernation` |
| Hibernation expired but vault released | Hibernation duration elapsed | Set a longer duration or exit early |

### Variations

- **Indefinite hibernation**: Set a very large `duration_seconds`
- **Partial hibernation**: Combine with a regular check-in after exit

For full API reference, see [Vault Hibernation](hibernation.md).

---

## UC-08: Beneficiary Conflict Resolution

### Description

Multiple parties claim the same vault as beneficiary after the owner becomes unreachable. The protocol's automated conflict resolution mechanism adjudicates the dispute.

### Actors

- **Multiple claimants**: Two or more parties claiming beneficiary status
- **Protocol**: Automated arbitration via on-chain logic

### Step-by-Step Instructions

**1. Claimants submit their claims**

```rust
contract.submit_beneficiary_claim(vault_id, claimant_address, evidence_hash);
```

**2. Resolution window opens**

The protocol holds a resolution period during which all claimants can submit evidence.

**3. Automated resolution runs**

```rust
contract.resolve_beneficiary_conflict(vault_id);
```

**4. Winning claimant receives funds**

The highest-ranked claimant (per the ranking algorithm) receives the release.

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Claim rejected | Missing required evidence | Submit a valid `evidence_hash` |
| Conflict not resolvable | Tied ranking scores | Appeals process; see [Conflict Resolution](beneficiary-conflict-resolution.md) |

### Variations

- **Pre-designated backup beneficiary**: Avoids conflicts entirely
- **Multi-sig conflict resolution**: Requires multiple approvals to resolve

For full details, see [Beneficiary Conflict Resolution](beneficiary-conflict-resolution.md).

---

## UC-09: Vault with Vesting Schedule

### Description

Funds are released to a beneficiary gradually over a vesting period rather than all at once, preventing large lump-sum transfers.

### Actors

- **Owner**: Sets up the vesting schedule
- **Beneficiary**: Receives vested amounts over time

### Step-by-Step Instructions

**1. Create vault**

```rust
let vault_id = contract.create_vault(beneficiary_address, check_in_interval);
```

**2. Configure vesting schedule**

```rust
// Example: linear vesting over 2 years
contract.set_vesting_schedule(vault_id, VestingSchedule {
    start_timestamp: env.ledger().timestamp(),
    duration_seconds: 2 * 365 * 24 * 60 * 60,
    cliff_seconds: 6 * 30 * 24 * 60 * 60, // 6-month cliff
    total_amount: 10_000_0000000_i128,
});
```

**3. Beneficiary claims vested amounts**

```rust
let claimable = contract.get_vested_amount(vault_id);
contract.claim_vested(vault_id);
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| Zero claimable before cliff | Cliff period not elapsed | Wait for cliff to pass |
| Vesting paused | Vault in hibernation | Exit hibernation to resume |

For full API reference, see [Vesting Schedules](vesting-schedules.md).

---

## UC-10: Emergency TTL Borrowing

### Description

An owner realizes they are about to miss a check-in deadline and borrows TTL from another vault they own to extend the at-risk vault's expiry.

### Actors

- **Owner**: Controls both the borrower and lender vaults

### Prerequisites

- Owner controls at least two vaults
- Lender vault has sufficient remaining TTL

### Step-by-Step Instructions

**1. Check TTL remaining on both vaults**

```rust
let borrower_ttl = contract.get_ttl_remaining(borrower_vault_id);
let lender_ttl = contract.get_ttl_remaining(lender_vault_id);
```

**2. Borrow TTL**

```rust
let borrow_seconds: u64 = 7 * 24 * 60 * 60; // borrow 7 days
contract.borrow_ttl(
    borrower_vault_id,
    lender_vault_id,
    owner_address,
    borrow_seconds,
)?;
```

**3. Verify the borrow**

```rust
let borrow_record = contract.get_ttl_borrow(borrower_vault_id);
// Some(TtlBorrowRecord) confirms the borrow
```

**4. Repay the borrow when possible**

```rust
contract.repay_ttl_borrow(borrower_vault_id, owner_address)?;
```

### Troubleshooting

| Problem | Cause | Solution |
|---|---|---|
| `InsufficientTtl` error | Lender vault doesn't have enough TTL | Choose a lender with more remaining TTL |
| Cannot repay | Borrow already repaid or expired | Check `get_ttl_borrow()` for current status |

### Variations

- **Multiple borrows**: Borrow from different lender vaults in sequence
- **Accelerated expiry**: Use `accelerate_ttl_decay()` to voluntarily shorten a vault instead

For full details, see [TTL & State Archival Logic](ttl-logic.md).
