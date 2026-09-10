# Repository Layout

This repository is split into **two independent Cargo workspaces** that share
one git history but build, test, lint, and release separately.

```
heirloom-contracts-backend/
├── contracts/          ← Workspace A — on-chain (Soroban / WASM)
│   ├── Cargo.toml       (workspace root: members ttl_vault, zk_verifier, sbt)
│   ├── Cargo.lock
│   ├── ttl_vault/       the micro-endowment check-in vault contract
│   ├── zk_verifier/     zero-knowledge / lattice proof verifier
│   └── sbt/             soulbound-token credential contract
│
├── backend/            ← Workspace B — off-chain HTTP/GraphQL/WebSocket service
│   ├── Cargo.toml       (standalone workspace, single member)
│   ├── Cargo.lock
│   ├── Dockerfile
│   └── src/
│
├── docs/               shared documentation
├── scripts/            build + deploy helpers (contracts)
└── .github/workflows/  CI runs the two workspaces as separate jobs
```

There is **no `Cargo.toml` at the repository root** — `cargo` commands must be
run from inside `contracts/` or `backend/`.

## Why the split

`backend/` has zero dependency on `soroban-sdk` or any contract crate; it only
speaks to the chain over RPC. Keeping them in one workspace forced every
`cargo` invocation to resolve both dependency trees (Soroban host + `ring` +
`async-graphql` + …) and pinned both to a single `[profile.release]` tuned for
WASM size. Splitting lets each side pick its own toolchain constraints,
release profile, lint set, and CI cadence.

## Working in each side

| Task              | Contracts                                  | Backend                         |
|-------------------|--------------------------------------------|---------------------------------|
| Build             | `cd contracts && cargo build`              | `cd backend && cargo build`     |
| Build WASM        | `./scripts/build.sh` (from repo root)      | n/a                             |
| Test              | `cd contracts && cargo test`               | `cd backend && cargo test`      |
| Lint              | `cd contracts && cargo clippy -- -D warnings` | `cd backend && cargo clippy --all-targets -- -D warnings` |
| Run the service   | n/a                                        | `cd backend && cargo run`       |
| Docker (service)  | n/a                                        | `docker-compose up -d`          |

`deny.toml`, `.clippy.toml`, and `.gitleaks.toml` stay at the repo root and are
shared. `cargo deny` is invoked per workspace with `--config ../deny.toml`.

## One-time lockfile finalization

`contracts/Cargo.lock` and `backend/Cargo.lock` were seeded from the old shared
root lockfile, so each currently lists some packages the other workspace no
longer uses. The first `cargo build`/`cargo test` in each directory (with
network access) prunes its lockfile in place **without upgrading any pinned
version**. Commit the two pruned lockfiles, after which `--locked` can be
re-added to the CI build steps and the Dockerfile.

```bash
cd contracts && cargo metadata --format-version 1 >/dev/null && cd ..
cd backend   && cargo metadata --format-version 1 >/dev/null && cd ..
git add contracts/Cargo.lock backend/Cargo.lock
```

## Release profiles

- **contracts** — `opt-level = "z"`, `lto = true`, `panic = "abort"`,
  `overflow-checks = true` (unchanged; this is the on-chain WASM profile).
- **backend** — `opt-level = 3`, `panic = "unwind"`, `overflow-checks = true`.
  Before the split the service inherited the WASM profile; `panic = "unwind"`
  is restored so a panic in one request handler cannot abort the whole
  process.
