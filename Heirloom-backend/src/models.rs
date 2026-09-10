use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── WebSocket authentication ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClaims {
    pub sub: String,
    pub vault_ids: Vec<String>,
    pub exp: usize,
}

// ── Locale support ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    En,
    Es,
    Fr,
    De,
}

// ── Notification models ──────────────────────────────────────────────────────

// ── Legacy reminder API models (axum + Db contract) ───────────────────────

/// Reminder notification channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Email,
    Sms,
    Push,
}

/// Reminder frequency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    Once,
    Daily,
    Weekly,
    Hourly,
    Monthly,
}

pub type VaultNotificationPreferences = NotificationPreferences;

/// Persisted reminder preferences stored by `Db`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderPreferences {
    pub vault_id: u64,
    pub channels: Vec<Channel>,
    pub hours_before_expiry: u32,
    pub frequency: Frequency,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Request body for setting reminder preferences.
#[derive(Debug, Deserialize, Clone)]
pub struct SetPreferencesRequest {
    pub channels: Vec<Channel>,
    pub hours_before_expiry: u32,
    pub frequency: Frequency,
}

/// Notification type sent to a device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    ExpiryWarning,
    CheckInReminder,
    VaultReleased,
    VaultPaused,
    /// A vault passkey is approaching its expiry timestamp (#560).
    PasskeyExpiringSoon,
    /// A vault passkey has already expired (#560).
    PasskeyExpired,
}

/// Delivery status of a single notification attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Sent,
    Failed,
    Retrying,
}

/// A single attempt entry within a reminder delivery log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttempt {
    pub attempt: u32,
    pub attempted_at: DateTime<Utc>,
    pub error: String,
}

/// Per-notification retry log stored by notification ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderDeliveryLog {
    pub notification_id: String,
    pub vault_id: String,
    pub owner: String,
    pub status: DeliveryStatus,
    pub attempts: Vec<DeliveryAttempt>,
    /// When the next retry should fire (None if not retrying).
    pub next_retry_at: Option<DateTime<Utc>>,
}

/// A registered device push token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceToken {
    pub owner: String,
    pub token: String,
    /// "ios" | "android" | "web"
    pub platform: String,
    pub registered_at: DateTime<Utc>,
}

/// Per-owner notification preferences (used by legacy scheduler/reminder engine).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub owner: String,
    pub expiry_warning_enabled: bool,
    pub check_in_reminder_enabled: bool,
    pub vault_released_enabled: bool,
    /// Hours before expiry to send the warning (default 24).
    pub warning_hours_before: u64,
    pub locale: Option<Locale>,
    pub preferred_channel: Option<NotificationChannel>,
    pub fallback_channel: Option<NotificationChannel>,
    pub unsubscribed: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            owner: String::new(),
            expiry_warning_enabled: true,
            check_in_reminder_enabled: true,
            vault_released_enabled: true,
            warning_hours_before: 24,
            locale: None,
            preferred_channel: None,
            fallback_channel: None,
            unsubscribed: false,
        }
    }
}

// ── Unsubscribe support (#828) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeToken {
    pub token: String,
    pub owner: String,
    pub created_at: DateTime<Utc>,
}

// ── Channel fallback delivery log (#827) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDeliveryLog {
    pub notification_id: String,
    pub channel: NotificationChannel,
    pub status: DeliveryStatus,
    pub attempted_at: DateTime<Utc>,
    pub error: Option<String>,
}

/// A scheduled notification (pending delivery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledNotification {
    pub id: String,
    pub vault_id: String,
    pub owner: String,
    pub notification_type: NotificationType,
    /// Unix timestamp when this should fire.
    pub scheduled_at: DateTime<Utc>,
    pub status: DeliveryStatus,
    pub max_retry_attempts: u32,
    pub sent_at: Option<DateTime<Utc>>,
    /// Set for `PasskeyExpiringSoon` / `PasskeyExpired` notifications — identifies
    /// which passkey this notification is about (#560).
    pub passkey_hash: Option<String>,
    /// Approximate hours remaining until expiry at scheduling time, used to
    /// populate the notification body (#560).
    pub ttl_hours: Option<u64>,
}

/// Delivery record written after each send attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub notification_id: String,
    pub vault_id: String,
    pub owner: String,
    pub notification_type: NotificationType,
    pub status: DeliveryStatus,
    pub sent_at: DateTime<Utc>,
    /// FCM message ID on success, error string on failure.
    pub provider_response: String,
}

/// Request body for `POST /notifications/register`.
#[derive(Debug, Deserialize)]
pub struct RegisterTokenRequest {
    pub owner: String,
    pub token: String,
    pub platform: String,
}

/// Request body for `PUT /notifications/preferences`.
#[derive(Debug, Deserialize)]
pub struct UpdatePreferencesRequest {
    pub owner: String,
    pub expiry_warning_enabled: Option<bool>,
    pub check_in_reminder_enabled: Option<bool>,
    pub vault_released_enabled: Option<bool>,
    pub warning_hours_before: Option<u64>,
    pub locale: Option<Locale>,
}

// ── Existing models (unchanged) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub id: String,
    pub owner: String,
    pub beneficiary: String,
    pub balance: i128,
    pub check_in_interval: u64,
    pub last_check_in: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub status: VaultStatus,
    pub ttl_remaining: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VaultStatus {
    Active,
    Expired,
    Released,
    Paused,
}

// ── TTL Insurance models ───────────────────────────────────────────────────

/// TTL insurance policy parameters purchased by a vault owner.
///
/// When enabled, the backend scheduler can automatically extend TTL once the
/// owner is considered inactive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtlInsurancePolicy {
    /// Vault id (matches `Vault.id` semantics in this backend).
    pub vault_id: u64,
    /// How much TTL to extend when triggered.
    pub extension_seconds: u64,
    /// Consider the owner inactive if no proof-of-life/check-in was recorded
    /// within this window.
    pub inactivity_threshold_seconds: u64,
    /// Whether this policy is currently active.
    pub enabled: bool,
    pub purchased_at: DateTime<Utc>,
    pub last_extended_at: Option<DateTime<Utc>>,
}

/// Persisted owner activity signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerActivity {
    pub owner_id: u64,
    pub last_active_at: DateTime<Utc>,
}

/// POST body to purchase/enable a TTL insurance policy.
#[derive(Debug, Deserialize, Clone)]
pub struct PurchaseTtlInsuranceRequest {
    pub extension_seconds: u64,
    pub inactivity_threshold_seconds: u64,
}

/// POST body to record owner activity (proof-of-life).
#[derive(Debug, Deserialize, Clone)]
pub struct RecordOwnerActivityRequest {
    pub owner_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEvent {
    pub vault_id: String,
    pub event_type: EventType,
    pub timestamp: DateTime<Utc>,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    CheckIn,
    TtlUpdate,
    StatusChange,
    Deposit,
    Withdrawal,
    Release,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub owner: Option<String>,
    pub beneficiary: Option<String>,
    pub status: Option<VaultStatus>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub vaults: Vec<Vault>,
    pub total: u32,
    pub page: u32,
    pub limit: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComparisonResult {
    pub vaults: Vec<Vault>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExportData {
    pub vault: Vault,
    pub history: Vec<VaultEvent>,
    pub audit_log: Vec<AuditEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub actor: String,
    pub details: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub message_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComplianceReport {
    pub vault_id: String,
    pub owner: String,
    pub beneficiary: String,
    pub report_generated_at: DateTime<Utc>,
    pub fund_movements: Vec<FundMovement>,
    pub beneficiary_changes: Vec<BeneficiaryChange>,
    pub ttl_history: Vec<TtlEvent>,
    pub total_deposits: i128,
    pub total_withdrawals: i128,
    pub current_balance: i128,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FundMovement {
    pub timestamp: DateTime<Utc>,
    pub movement_type: String,
    pub amount: i128,
    pub balance_after: i128,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BeneficiaryChange {
    pub timestamp: DateTime<Utc>,
    pub old_beneficiary: String,
    pub new_beneficiary: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TtlEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub ttl_remaining: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VaultTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub check_in_interval: u64,
    pub recommended_for: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultTemplateList {
    pub templates: Vec<VaultTemplate>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateVaultFromTemplate {
    pub template_id: String,
    pub owner: String,
    pub beneficiary: String,
}

// ── Task 1: Analytics ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultAnalytics {
    pub total_vaults: u64,
    pub active_vaults: u64,
    pub average_ttl_seconds: f64,
    pub release_rate: f64, // fraction of vaults that are Released
    pub time_series: Vec<TimeSeriesPoint>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TimeSeriesPoint {
    pub date: String, // ISO-8601 date (YYYY-MM-DD)
    pub vaults_created: u64,
    pub vaults_released: u64,
}

// ── Per-Vault Analytics (#959) ───────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultDetailAnalytics {
    pub vault_id: String,
    pub ttl_history: Vec<TtlHistoryPoint>,
    pub check_in_frequency: CheckInFrequency,
    pub withdrawal_trends: WithdrawalTrends,
    pub beneficiary_status: BeneficiaryStatus,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TtlHistoryPoint {
    pub date: String,
    pub ttl_remaining_seconds: u64,
    pub event: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckInFrequency {
    pub average_interval_seconds: u64,
    pub total_check_ins: u64,
    pub next_deadline: String,
    pub days_until_deadline: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WithdrawalTrends {
    pub total_withdrawals: i128,
    pub withdrawal_count: u64,
    pub average_withdrawal_amount: f64,
    pub last_withdrawal_date: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BeneficiaryStatus {
    pub beneficiary_address: String,
    pub is_active: bool,
    pub vault_status: String,
    pub can_receive_funds: bool,
}

// ── Task 2: Backup & Recovery ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultBackup {
    pub backup_id: String,
    pub vault_id: String,
    pub created_at: DateTime<Utc>,
    /// AES-GCM encrypted JSON of the vault state (base64-encoded)
    pub encrypted_payload: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub backup_id: String,
    /// The same key used during backup (base64-encoded 32-byte key)
    pub encryption_key: String,
}

/// Request body for `POST /admin/validate-backup` (#81): validate the
/// integrity of a base64-encoded backup payload.
#[derive(Debug, Deserialize)]
pub struct BackupValidateRequest {
    pub backup_id: String,
    pub data_base64: String,
}

// ── Task 3: Sharing & Collaboration ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SharePermission {
    ViewOnly,
    Edit,
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultShare {
    pub share_id: String,
    pub vault_id: String,
    pub shared_with: String, // address or email
    pub permission: SharePermission,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShareRequest {
    pub shared_with: String,
    pub permission: SharePermission,
}

// ── Share tokens (temporary access tokens for read-only sharing) ─────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareToken {
    pub token: String,
    pub share_id: String,
    pub vault_id: String,
    pub shared_with: String,
    pub permission: SharePermission,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Deserialize)]
pub struct GenerateTokenRequest {
    pub shared_with: String,
    pub permission: Option<SharePermission>,
    /// Seconds until the token expires (default 604800 = 7 days).
    pub expiry_seconds: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShareTokenResponse {
    pub share: VaultShare,
    pub token: ShareToken,
    pub access_url: String,
}

#[derive(Debug, Deserialize)]
pub struct RevokeTokenRequest {
    pub token: String,
}

// ── Task 4: Notification Preferences ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannel {
    Email,
    Sms,
    Push,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationFrequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NotificationPreferencesRequest {
    pub channels: Vec<NotificationChannel>,
    pub frequency: NotificationFrequency,
}

// ── Vault Notification Subscription System ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionChannel {
    Email,
    Sms,
    Webhook,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionFrequency {
    Once,
    Daily,
    Weekly,
    Hourly,
    Monthly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subscription {
    pub vault_id: u64,
    pub owner: String,
    pub channels: Vec<SubscriptionChannel>,
    pub frequency: SubscriptionFrequency,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SetSubscriptionRequest {
    pub owner: String,
    pub channels: Vec<SubscriptionChannel>,
    pub frequency: SubscriptionFrequency,
}

// ── Idempotency Key support (#825) ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub key: String,
    pub response_body: String,
    pub status_code: u16,
    pub created_at: DateTime<Utc>,
}

// ── Release Simulator models ─────────────────────────────────────────────────

/// The scenario to simulate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioType {
    /// Owner never checks in again — release is immediate at TTL expiry.
    NoCheckIns,
    /// Owner checks in consistently at their configured interval.
    ConsistentCheckIns,
    /// Owner misses one or more specific check-in dates before stopping.
    MissedCheckInDates,
}

impl std::fmt::Display for ScenarioType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScenarioType::NoCheckIns => write!(f, "no_check_ins"),
            ScenarioType::ConsistentCheckIns => write!(f, "consistent_check_ins"),
            ScenarioType::MissedCheckInDates => write!(f, "missed_check_in_dates"),
        }
    }
}

/// Query parameters for the simulate-release endpoint.
#[derive(Debug, Deserialize)]
pub struct SimulateReleaseQuery {
    /// Comma-separated list of scenarios to run.
    /// e.g. `scenarios=no_check_ins,consistent_check_ins`
    /// Defaults to all three scenarios if omitted.
    pub scenarios: Option<String>,
    /// For `missed_check_in_dates`: number of consecutive missed check-ins
    /// before the owner stops (defaults to 1).
    pub missed_count: Option<u32>,
}

/// Projected outcome for a single scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Which scenario this result belongs to.
    pub scenario: ScenarioType,
    /// Human-readable description of the scenario.
    pub description: String,
    /// Projected UTC timestamp when the vault will release.
    pub projected_release_at: DateTime<Utc>,
    /// Seconds from now until the projected release.
    pub seconds_until_release: i64,
    /// Confidence level: "high", "medium", or "low".
    pub confidence: String,
    /// Optional extra notes about this scenario's assumptions.
    pub notes: String,
}

/// Response body for GET /api/vaults/{id}/simulate-release.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulateReleaseResponse {
    pub vault_id: String,
    /// Current TTL remaining in seconds (None if already expired/released).
    pub current_ttl_remaining: Option<u64>,
    /// The vault's configured check-in interval in seconds.
    pub check_in_interval: u64,
    /// Timestamp of the last recorded check-in.
    pub last_check_in: DateTime<Utc>,
    /// Simulation results, one per requested scenario.
    pub scenarios: Vec<ScenarioResult>,
    /// When this simulation was generated.
    pub simulated_at: DateTime<Utc>,
}

// ── Audit Log persistence (#961) ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub user_id: String,
    pub action: String,
    pub resource: String,
    pub result: String,
    pub ip_address: String,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AuditLogQuery {
    pub user_id: Option<String>,
    pub action: Option<String>,
    pub resource: Option<String>,
    pub result: Option<String>,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// ── Cache layer models ───────────────────────────────────────────────────────

/// Lightweight read-projection of a Vault, cached separately from the full
/// `Vault` struct to reduce allocation when only summary data is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSummary {
    pub vault_id: String,
    pub owner: String,
    pub status: VaultStatus,
    pub ttl_remaining: Option<u64>,
    pub balance: i128,
}

impl From<&Vault> for VaultSummary {
    fn from(v: &Vault) -> Self {
        Self {
            vault_id: v.id.clone(),
            owner: v.owner.clone(),
            status: v.status.clone(),
            ttl_remaining: v.ttl_remaining,
            balance: v.balance,
        }
    }
}

/// Response body returned by `POST /api/cache/invalidate/{vault_id}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheInvalidateResponse {
    pub vault_id: String,
    pub invalidated: bool,
}

// ── 2FA models (#965) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TwoFactorMethod {
    Totp,
    Sms,
    Email,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwoFactorConfig {
    pub vault_id: String,
    pub method: TwoFactorMethod,
    pub enabled: bool,
    pub secret: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Enable2FARequest {
    pub method: TwoFactorMethod,
    pub phone: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Verify2FARequest {
    pub otp: String,
}

#[derive(Debug, Serialize)]
pub struct Enable2FAResponse {
    pub vault_id: String,
    pub method: TwoFactorMethod,
    pub secret: Option<String>,
    pub provisioning_uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TwoFactorStatusResponse {
    pub vault_id: String,
    pub enabled: bool,
    pub method: Option<TwoFactorMethod>,
    pub verified: bool,
    pub phone: Option<String>,
    pub email: Option<String>,
}

// ── #69: Multi-Tenancy Support ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantBilling {
    pub tenant_id: String,
    pub monthly_charge: i128,
    pub billing_cycle_start: DateTime<Utc>,
    pub billing_cycle_end: DateTime<Utc>,
    pub total_vaults: u32,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TenantContext {
    pub tenant_id: String,
    pub user_id: String,
}

// ── #70: Real-Time Collaboration Features ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialUpdate {
    pub id: String,
    pub vault_id: String,
    pub user_id: String,
    pub field: String,
    pub old_value: serde_json::Value,
    pub new_value: serde_json::Value,
    pub timestamp: DateTime<Utc>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalTransform {
    pub id: String,
    pub vault_id: String,
    pub user_id: String,
    pub operation: String,
    pub position: u32,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub conflict_id: String,
    pub vault_id: String,
    pub update1_id: String,
    pub update2_id: String,
    pub resolution_strategy: String,
    pub resolved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresence {
    pub user_id: String,
    pub vault_id: String,
    pub status: String,
    pub last_seen: DateTime<Utc>,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborativeSession {
    pub session_id: String,
    pub vault_id: String,
    pub created_at: DateTime<Utc>,
    pub participants: Vec<String>,
    pub is_active: bool,
}

// ── #71: Advanced Search with Full-Text Capabilities ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullTextSearchQuery {
    pub query: String,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub filters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchFacet {
    pub name: String,
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetValue {
    pub value: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullTextSearchResult {
    pub id: String,
    pub vault_id: String,
    pub title: String,
    pub snippet: String,
    pub relevance_score: f32,
    pub matched_fields: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FullTextSearchResponse {
    pub results: Vec<FullTextSearchResult>,
    pub total: u32,
    pub facets: Vec<SearchFacet>,
    pub query_time_ms: u64,
}

// ── #100: Data Retention Policies ───────────────────────────────────────────

/// Policy controlling how long a particular data type is kept before purging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataRetentionPolicy {
    /// Logical name of the data type (e.g. "audit_logs", "reminder_preferences").
    pub data_type: String,
    /// Number of days to retain records. 0 means retain forever.
    pub retention_days: u32,
    /// Whether the policy is actively enforced by the purge scheduler.
    pub enabled: bool,
    /// Human-readable description of this policy.
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating or updating a retention policy.
#[derive(Debug, Deserialize)]
pub struct UpsertRetentionPolicyRequest {
    pub retention_days: u32,
    pub enabled: Option<bool>,
    pub description: Option<String>,
}

/// A single entry in the deletion audit trail produced by the purge job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionDeletionLog {
    pub id: i64,
    pub data_type: String,
    pub deleted_rows: u64,
    pub purged_at: DateTime<Utc>,
    /// "system" for automated purges, user ID for manual purges.
    pub actor: String,
    /// Optional JSON details about the purge run.
    pub details: Option<serde_json::Value>,
}

/// An exception that exempts a specific record from normal retention purging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionException {
    pub id: i64,
    pub data_type: String,
    /// Opaque identifier of the record being exempted.
    pub record_id: String,
    /// Business reason for the exemption.
    pub reason: String,
    /// When the exemption itself expires (None = permanent).
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

/// Request body for registering a retention exception.
#[derive(Debug, Deserialize)]
pub struct CreateRetentionExceptionRequest {
    pub record_id: String,
    pub reason: String,
    /// Seconds until this exemption expires. None = permanent.
    pub expires_in_seconds: Option<u64>,
}

/// Response returned after running a manual purge.
#[derive(Debug, Serialize, Deserialize)]
pub struct PurgeRunResult {
    pub data_type: String,
    pub deleted_rows: u64,
    pub purged_at: DateTime<Utc>,
}

// ── #101: Encrypted Field Storage ───────────────────────────────────────────

/// A field value stored after AES-256-GCM encryption.
/// The ciphertext and nonce are base64-encoded for safe serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedField {
    /// Base64-encoded AES-256-GCM ciphertext.
    pub ciphertext: String,
    /// Base64-encoded 12-byte nonce used for this encryption.
    pub nonce: String,
    /// Key version used to encrypt this field (supports rotation).
    pub key_version: u32,
}

/// Metadata about an active or retired encryption key version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionKeyInfo {
    pub version: u32,
    pub status: EncryptionKeyStatus,
    pub created_at: DateTime<Utc>,
    /// When this key was rotated out (None if still active).
    pub rotated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionKeyStatus {
    Active,
    /// Key is being phased out; still usable for decryption but not encryption.
    Retiring,
    Retired,
}

/// Summary of a key-rotation operation.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeyRotationResult {
    pub previous_version: u32,
    pub new_version: u32,
    pub rotated_at: DateTime<Utc>,
    /// Number of encrypted records re-encrypted with the new key.
    pub records_re_encrypted: u64,
}

// ── #103: Secret Rotation Policy ────────────────────────────────────────────

/// Categories of secrets managed by the rotation policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SecretType {
    ApiKey,
    DatabasePassword,
    EncryptionKey,
    JwtSecret,
    WebhookSecret,
    RemindersApiKey,
}

/// Rotation schedule configuration for a specific secret type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRotationPolicy {
    pub secret_type: SecretType,
    /// How often this secret must be rotated (in days).
    pub rotation_interval_days: u32,
    /// Grace period (in hours) during which both old and new secrets are accepted.
    pub grace_period_hours: u32,
    /// Whether automated rotation is enabled.
    pub auto_rotate: bool,
    /// Notification channel(s) to alert when rotation is due / complete.
    pub notify_channels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for upserting a secret rotation policy.
#[derive(Debug, Deserialize)]
pub struct UpsertSecretRotationPolicyRequest {
    pub rotation_interval_days: u32,
    pub grace_period_hours: Option<u32>,
    pub auto_rotate: Option<bool>,
    pub notify_channels: Option<Vec<String>>,
}

/// A log entry recording that a secret was rotated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRotationLog {
    pub id: i64,
    pub secret_type: SecretType,
    pub rotated_at: DateTime<Utc>,
    /// "system" for automated rotations, user ID for manual rotations.
    pub actor: String,
    /// Whether the grace period is still active.
    pub grace_period_active: bool,
    /// When the grace period ends (None if not applicable).
    pub grace_period_ends_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

/// Status summary for a secret type.
#[derive(Debug, Serialize, Deserialize)]
pub struct SecretRotationStatus {
    pub secret_type: SecretType,
    pub last_rotated_at: Option<DateTime<Utc>>,
    pub next_rotation_due: Option<DateTime<Utc>>,
    pub is_overdue: bool,
    pub grace_period_active: bool,
    pub grace_period_ends_at: Option<DateTime<Utc>>,
}
