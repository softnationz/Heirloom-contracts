# Known Gaps

Documentation only — nothing here has been fixed. Each item is a diagnosed, unresolved issue found during the 2026-09-11/14 contracts cleanup session. No further action was taken beyond what's listed elsewhere in that session's commits.

## zk_verifier

- `is_kyc_status_claim`/`extract_kyc_status` (`Heirloom-contracts/zk_verifier/src/consistency.rs:228,243`) classify a claim's KYC status from its first byte alone, with no length check — an 8-byte age claim starting with `0x00` is misread as `KycStatus::Pending`. Traced to the original commit (`b20db70`, #33); wrong since first written, not lost to a later merge.
- `kyc_statuses_compatible` (`Heirloom-contracts/zk_verifier/src/consistency.rs:256`) implements only 2 of the 3 conflict rules its own doc comment states (`pending+approved`, `pending+rejected`, `approved+rejected` should all conflict) — the `matches!` pattern omits the `(Pending, Approved)` pair, so that combination is incorrectly treated as compatible. Same origin commit as above.

## sbt

- `Heirloom-contracts/sbt/src/types.rs` documents `migrate_sbt_metadata` (schema versioning, `CURRENT_SCHEMA_VERSION`/`COMPRESSION_SCHEMA_VERSION` consts) as a real function, but no such function exists anywhere in current `lib.rs` — orphaned documentation for a feature that's either lost or was never built; not yet investigated via `git log -S`.

## ttl_vault

- `Heirloom-contracts/ttl_vault/src/test.rs` (6,701 lines, not wired into the crate) has a genuine Rust syntax error around line 3706: an orphaned code fragment referencing `vault_id`/`owner`/`beneficiary`/`env`/`client` with no enclosing `fn`/`setup()` call, immediately after a cleanly-closed test function — consistent with a merge dropping a test function's header while its body survived. Compilation stops at this error, so there may be further issues in the file beyond this point that haven't been seen yet.

## Heirloom-backend

- `cargo clippy --all-targets --all-features` reports 220 warnings (not run under `-D warnings`, so these don't fail CI's clippy step, which is scoped elsewhere). Breakdown of the largest categories: 44 "Err-variant returned from this function is very large", 39 "unused async for function with no await statements", 22 "redundant closure", 14 "map().unwrap_or() on an Option value", 13 "variables can be used directly in the format! string", 5 "casting u64 to i64 may wrap around the value", 5 "boolean to int conversion using if", 4 "returning a str unnecessarily tied to the lifetime of arguments". Pre-existing, not triaged.

## Appendix: systemic-audit agent report (appended as-is, not verified or acted on)

A background agent was asked to sweep all three contract crates and Heirloom-backend for symbols referenced in tests/docs but missing from source, categorizing each via `git log -S` as Recoverable / Designed-but-never-built / Stale-test-against-superseded-API. It hit a rate limit mid-run and was resumed once; its final report, reproduced verbatim below, is a recap of findings already surfaced elsewhere in this session (all git-verified) plus an explicit list of what it did *not* get to. Treat the "not completed" section as the actual gap — nothing in the table is new information.

> Given the scope here (103 doc files alone, plus exhaustive symbol cross-referencing across 3 crates + backend), a true exhaustive sweep isn't something I can honestly claim complete right now. Here's exactly what I have, clearly marked by confidence level — sending as-is per the status check.
>
> ### Consolidated list — confirmed findings (all verified in this session)
>
> | Crate | Symbol(s) | Category | Evidence |
> |---|---|---|---|
> | zk_verifier | 13 dispute/version/hierarchy methods | **Recoverable** | `git log -S` → 329e00b, ancestor of `main`. Now implemented (not recovered — per instruction, reimplemented from doc). Fixed. |
> | zk_verifier | `ATTESTATION_RECORD_FIELD_ORACLE`, `MASKED_ORACLE_STRKEY`, missing `next_check_due` field | Pre-existing bugs, not symbol-loss | Fixed alongside Phase 2. |
> | sbt | `MAX_COMPOSITION_DEPTH`, `SbtError::InvalidCompositionGraph` | **Recoverable** | `git log -S` → d18959c, dropped at 67bc951. Fixed (value 32, confirmed via history). |
> | sbt | `mint_sbt`, `migrate_sbt_metadata`, `get_schema_version`, raw `DataKey::Sbt` (in deleted `sbt/src/test.rs`) | **Stale test, superseded API** | Current code uses `mint`; no evidence any of these three ever coexisted with today's data model. Deleted. |
> | ttl_vault | `clawback_post_release`, `mark_beneficiary_released` | **Recoverable** | `git log -S` → c71e88e (#526), ancestor of `main`. |
> | ttl_vault | `apply_rounding`, `set_rounding_mode`, `get_rounding_mode` | **Recoverable** | `git log -S` → 9779b62 (#524), ancestor of `main`. |
> | ttl_vault | `swap_allocations` | **Recoverable** | `git log -S` → b552ab0 (#528), ancestor of `main`. |
> | ttl_vault | `grace_period_tests.rs`, `trigger_release_bench_tests.rs`, `clone_vault_tests.rs` | **Stale test, superseded API** | All 3 reference real, still-present methods (`check_in`, `deposit`, `set_beneficiaries`, etc.) but with old signatures/testutils APIs (`check_in` gained `passkey_hash`/`nonce`; old `MockAuthorizationEntry`/`env.set_auth` no longer exist; `BeneficiaryEntry` gained `minimum_threshold`). Not missing code — just old call sites. |
> | zk_verifier | `consistency.rs`'s KYC length-check gap and `Pending`/`Approved` conflict-matrix gap | **Neither** — genuine authoring bugs | Single-commit history (`b20db70`, #33) — wrong since day one, nothing lost. |
>
> ### Not completed — flagging honestly rather than guessing
>
> - **`sbt/src/types.rs`'s orphaned `migrate_sbt_metadata` doc** and **`ttl_vault/src/test.rs`'s line-3706 syntax corruption**: still on hold as instructed, not reinvestigated.
> - **A genuinely exhaustive doc↔code sweep**: `docs/` has 103 files. Most have not been cross-referenced (e.g., `docs/sbt.md`, `docs/sbt-advanced-features.md`, `docs/beneficiary-*.md` ×10, `docs/passkeys.md`, etc.) against current `pub fn` lists.
> - **Heirloom-backend**: not checked at all this pass — its docs vs `src/` handlers is untouched.
> - **zk_verifier/sbt beyond what's already surfaced**: everything above came from issues this session already hit directly (compile errors, `should_panic` mismatches); no independent grep of every wired-in test file's symbols against `lib.rs` from scratch to check for anything *not* already surfaced as an error.

**Practical read:** the audit did not reach a genuinely exhaustive sweep (103 docs files, Heirloom-backend, and independent-of-error-message grepping were all out of scope for what it completed). Everything it *did* confirm duplicates findings already fixed or already listed above in this file. If a real exhaustive audit is wanted later, scope it as its own focused task rather than a background pass alongside other work.
