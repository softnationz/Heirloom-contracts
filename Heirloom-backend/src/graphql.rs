//! GraphQL API alternative to REST (#66).
//!
//! Exposes a `/graphql` endpoint (and a `/graphql/playground` IDE) powered by
//! [`async-graphql`] and [`async-graphql-axum`].
//!
//! # Schema overview
//!
//! ```graphql
//! type Query {
//!   vault(id: String!): Vault
//!   vaults(owner: String, status: String, page: Int, limit: Int): VaultPage
//!   vaultEvents(vaultId: String!): [VaultEvent!]!
//! }
//!
//! type Mutation {
//!   createVault(owner: String!, beneficiary: String!, checkInInterval: Int!): Vault!
//!   checkIn(vaultId: String!): Vault!
//! }
//! ```

use async_graphql::{
    http::GraphiQLSource, Context, EmptySubscription, InputObject, Object, Schema, SimpleObject,
};
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    Json,
};
use chrono::{DateTime, Utc};

use crate::db::{EventStore, VaultStore};
use crate::models::{Vault as DomainVault, VaultEvent as DomainEvent, VaultStatus};

// ── GraphQL types (mirrors of domain models) ─────────────────────────────────

/// A vault as returned by the GraphQL API.
#[derive(SimpleObject, Clone)]
pub struct GqlVault {
    pub id: String,
    pub owner: String,
    pub beneficiary: String,
    /// Balance in stroops (i128 serialised as String to avoid JS precision loss).
    pub balance: String,
    pub check_in_interval: i64,
    pub last_check_in: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub ttl_remaining: Option<i64>,
}

impl From<DomainVault> for GqlVault {
    fn from(v: DomainVault) -> Self {
        GqlVault {
            id: v.id,
            owner: v.owner,
            beneficiary: v.beneficiary,
            balance: v.balance.to_string(),
            check_in_interval: v.check_in_interval as i64,
            last_check_in: v.last_check_in,
            created_at: v.created_at,
            status: format!("{:?}", v.status).to_lowercase(),
            ttl_remaining: v.ttl_remaining.map(|t| t as i64),
        }
    }
}

/// Paginated vault list.
#[derive(SimpleObject)]
pub struct VaultPage {
    pub vaults: Vec<GqlVault>,
    pub total: i32,
    pub page: i32,
    pub limit: i32,
}

/// A vault event.
#[derive(SimpleObject, Clone)]
pub struct GqlVaultEvent {
    pub vault_id: String,
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    /// Event payload as a JSON string.
    pub data: String,
}

impl From<DomainEvent> for GqlVaultEvent {
    fn from(e: DomainEvent) -> Self {
        GqlVaultEvent {
            vault_id: e.vault_id,
            event_type: format!("{:?}", e.event_type).to_lowercase(),
            timestamp: e.timestamp,
            data: e.data.to_string(),
        }
    }
}

/// Input for creating a new vault.
#[derive(InputObject)]
pub struct CreateVaultInput {
    pub owner: String,
    pub beneficiary: String,
    pub check_in_interval: i64,
}

// ── Schema context ────────────────────────────────────────────────────────────

pub struct QueryRoot;
pub struct MutationRoot;

pub type HeirloomSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

#[Object]
impl QueryRoot {
    /// Fetch a single vault by ID.
    async fn vault(&self, ctx: &Context<'_>, id: String) -> Option<GqlVault> {
        let store = ctx.data_unchecked::<VaultStore>();
        let vaults = store.lock().unwrap();
        vaults.get(&id).cloned().map(GqlVault::from)
    }

    /// List vaults with optional filters and pagination.
    async fn vaults(
        &self,
        ctx: &Context<'_>,
        owner: Option<String>,
        status: Option<String>,
        page: Option<i32>,
        limit: Option<i32>,
    ) -> VaultPage {
        let store = ctx.data_unchecked::<VaultStore>();
        let page = page.unwrap_or(1).max(1) as usize;
        let limit = limit.unwrap_or(10).clamp(1, 100) as usize;
        let offset = (page - 1) * limit;

        let vaults_guard = store.lock().unwrap();
        let filtered: Vec<GqlVault> = vaults_guard
            .values()
            .filter(|v| {
                if let Some(ref o) = owner {
                    if v.owner != *o {
                        return false;
                    }
                }
                if let Some(ref s) = status {
                    let v_status = format!("{:?}", v.status).to_lowercase();
                    if v_status != *s {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .map(GqlVault::from)
            .collect();

        let total = filtered.len() as i32;
        let page_items: Vec<GqlVault> = filtered.into_iter().skip(offset).take(limit).collect();

        VaultPage {
            vaults: page_items,
            total,
            page: page as i32,
            limit: limit as i32,
        }
    }

    /// List all events for a vault.
    async fn vault_events(&self, ctx: &Context<'_>, vault_id: String) -> Vec<GqlVaultEvent> {
        let store = ctx.data_unchecked::<EventStore>();
        let events = store.lock().unwrap();
        events
            .iter()
            .filter(|e| e.vault_id == vault_id)
            .cloned()
            .map(GqlVaultEvent::from)
            .collect()
    }
}

#[Object]
impl MutationRoot {
    /// Create a new vault entry in the in-memory store.
    async fn create_vault(
        &self,
        ctx: &Context<'_>,
        input: CreateVaultInput,
    ) -> async_graphql::Result<GqlVault> {
        if input.owner.is_empty() {
            return Err("owner must not be empty".into());
        }
        if input.beneficiary.is_empty() {
            return Err("beneficiary must not be empty".into());
        }
        if input.check_in_interval <= 0 {
            return Err("check_in_interval must be positive".into());
        }

        let vault = DomainVault {
            id: uuid::Uuid::new_v4().to_string(),
            owner: input.owner,
            beneficiary: input.beneficiary,
            balance: 0,
            check_in_interval: input.check_in_interval as u64,
            last_check_in: Utc::now(),
            created_at: Utc::now(),
            status: VaultStatus::Active,
            ttl_remaining: Some(input.check_in_interval as u64),
        };

        let store = ctx.data_unchecked::<VaultStore>();
        let mut vaults = store.lock().unwrap();
        vaults.insert(vault.id.clone(), vault.clone());

        Ok(GqlVault::from(vault))
    }

    /// Record a check-in for an existing vault (resets TTL).
    async fn check_in(
        &self,
        ctx: &Context<'_>,
        vault_id: String,
    ) -> async_graphql::Result<GqlVault> {
        let store = ctx.data_unchecked::<VaultStore>();
        let mut vaults = store.lock().unwrap();

        let vault = vaults.get_mut(&vault_id).ok_or("vault not found")?;

        vault.last_check_in = Utc::now();
        vault.ttl_remaining = Some(vault.check_in_interval);

        Ok(GqlVault::from(vault.clone()))
    }
}

// ── Schema builder ────────────────────────────────────────────────────────────

/// Build the [`HeirloomSchema`] with vault and event stores injected as context data.
pub fn build_schema(vault_store: VaultStore, event_store: EventStore) -> HeirloomSchema {
    Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(vault_store)
        .data(event_store)
        .finish()
}

// ── Axum handlers ─────────────────────────────────────────────────────────────

/// `POST /graphql` — execute a GraphQL query or mutation.
///
/// Implemented directly on `async_graphql`'s request/response types rather
/// than via `async-graphql-axum`, whose extractors are tied to a different
/// axum major version than the one this backend uses.
pub async fn graphql_handler(
    State(schema): State<HeirloomSchema>,
    Json(req): Json<async_graphql::Request>,
) -> Json<async_graphql::Response> {
    Json(schema.execute(req).await)
}

/// `GET /graphql/playground` — serve the GraphiQL IDE.
pub async fn graphql_playground() -> impl IntoResponse {
    Html(GraphiQLSource::build().endpoint("/graphql").finish())
}
