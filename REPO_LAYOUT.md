# Repository Layout

This repository is split into **two independent Cargo workspaces** that share
one git history but build, test, lint, and release separately.

```
./
├── Heirloom-contracts/  ← Workspace A — on-chain (Soroban / WASM)
│   ├── Cargo.toml        (workspace root: members ttl_vault, zk_verifier, sbt)
│   ├── Cargo.lock
│   ├── ttl_vault/        the micro-endowment check-in vault contract
│   ├── zk_verifier/      zero-knowledge / lattice proof verifier
│   └── sbt/              soulbound-token credential contract
│
├── Heirloom-backend/    ← Workspace B — off-chain HTTP/GraphQL/WebSocket service
│   ├── Cargo.toml        (standalone workspace, single member)
│   ├── Cargo.lock
│   ├── Dockerfile
│   └── src/
│
├── docs/                shared documentation
├── scripts/              build + deploy helpers (contracts)
└── .github/workflows/    CI runs the two workspaces as separate jobs
```

There is **no `Cargo.toml` at the repository root** — `cargo` commands must be
run from inside `Heirloom-contracts/` or `Heirloom-backend/`.

## Why the split

`Heirloom-backend/` has zero dependency on `soroban-sdk` or any contract
crate; it only speaks to the chain over RPC. Keeping them in one workspace
forced every `cargo` invocation to resolve both dependency trees (Soroban
host + `ring` + `async-graphql` + …) and pinned both to a single
`[profile.release]` tuned for WASM size. Splitting lets each side pick its
own toolchain constraints, release profile, lint set, and CI cadence.

## Working in each side

| Task              | Contracts                                              | Backend                                    |
|-------------------|---------------------------------------------------------|---------------------------------------------|
| Build             | `cd Heirloom-contracts && cargo build`                  | `cd Heirloom-backend && cargo build`         |
| Build WASM        | `./scripts/build.sh` (from repo root)                   | n/a                                          |
| Test              | `cd Heirloom-contracts && cargo test`                   | `cd Heirloom-backend && cargo test`          |
| Lint              | `cd Heirloom-contracts && cargo clippy -- -D warnings`  | `cd Heirloom-backend && cargo clippy --all-targets -- -D warnings` |
| Run the service   | n/a                                                      | `cd Heirloom-backend && cargo run`           |
| Docker (service)  | n/a                                                      | `docker-compose up -d`                       |

`deny.toml`, `.clippy.toml`, and `.gitleaks.toml` stay at the repo root and are
shared. `cargo deny` is invoked per workspace with `--config ../deny.toml`.

## Release profiles

- **Heirloom-contracts** — `opt-level = "z"`, `lto = true`, `panic = "abort"`,
  `overflow-checks = true` (unchanged; this is the on-chain WASM profile).
- **Heirloom-backend** — `opt-level = 3`, `panic = "unwind"`, `overflow-checks = true`.
  Before the split the service inherited the WASM profile; `panic = "unwind"`
  is restored so a panic in one request handler cannot abort the whole
  process.
