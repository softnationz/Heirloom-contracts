# syntax=docker/dockerfile:1
#
# Build context for this Dockerfile is the backend/ directory. The backend is a
# self-contained Cargo workspace (single member) with no dependency on the
# on-chain contracts in ../contracts, so the context no longer needs to be the
# repository root. docker-compose.yml / docker-compose.override.yml set
# `context: ./backend`, `dockerfile: Dockerfile` accordingly.
#
# Runtime environment variables (see ../.env.example):
#   ALLOWED_ORIGINS, MIN_CONTRACT_VERSION, DB_POOL_MIN, DB_POOL_MAX,
#   DB_POOL_TIMEOUT_SECS, NOTIFICATION_SCHEDULER_INTERVAL_SECS,
#   FCM_SERVER_KEY, FCM_PROJECT_ID, REMINDER_EMAIL_API_KEY, REMINDER_SMS_API_KEY,
#   REMINDER_ENCRYPTION_SECRET
# Additional vars read by the backend but not yet documented in .env.example:
#   NODE_ID, CONSENSUS_STRATEGY, REDIS_URL, ADMIN_API_KEY
# NOTE: the backend currently opens SQLite at ":memory:" (src/main.rs) --
# DATABASE_URL / the Postgres service in docker-compose.yml are not read by this
# binary. Vault data does not persist across container restarts.

ARG RUST_VERSION=1.96.1

# ── Base: shared toolchain + system deps for building ───────────────────────
FROM rust:${RUST_VERSION}-slim-bookworm AS base
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc \
        pkg-config \
      && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# ── Development: live-reload against the bind-mounted src/ ──────────────────
FROM base AS development
RUN cargo install cargo-watch --locked
COPY Cargo.toml Cargo.lock ./
COPY src ./src
EXPOSE 3000
CMD ["cargo", "watch", "--workdir", "/app", "-w", "src", "-x", "run"]

# ── Builder: compile the release binary ──────────────────────────────────────
FROM base AS builder
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# ── Runtime: minimal production image ────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
      && rm -rf /var/lib/apt/lists/* \
      && useradd --system --create-home --shell /usr/sbin/nologin heirloom

WORKDIR /app
COPY --from=builder /app/target/release/heirloom-protocol-backend /usr/local/bin/heirloom-protocol-backend

USER heirloom
EXPOSE 3000
ENV RUST_LOG=info

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

ENTRYPOINT ["/usr/local/bin/heirloom-protocol-backend"]
