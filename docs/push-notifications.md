# Push Notification Setup

Heirloom-Protocol uses **Firebase Cloud Messaging (FCM) HTTP v1 API** to deliver push notifications to iOS, Android, and web clients.

## Environment Variables

Add these to your `.env` (see `.env.example`):

```env
FCM_SERVER_KEY=<OAuth2 access token or legacy server key>
FCM_PROJECT_ID=<your-firebase-project-id>
NOTIFICATION_SCHEDULER_INTERVAL_SECS=60
```

Get your project ID from the [Firebase Console](https://console.firebase.google.com) → Project Settings.

## Notification Types

| Type | Trigger | Default |
|---|---|---|
| `expiry_warning` | TTL drops below `warning_hours_before` | enabled, 24h |
| `check_in_reminder` | Scheduled by owner | enabled |
| `vault_released` | Vault released to beneficiary | enabled |
| `vault_paused` | Vault paused | enabled |

## API Endpoints

### Register device token
```
POST /notifications/register
{ "owner": "<stellar-address>", "token": "<fcm-token>", "platform": "ios|android|web" }
```

### Unregister device token
```
DELETE /notifications/register
{ "owner": "<stellar-address>", "token": "<fcm-token>", "platform": "ios|android|web" }
```

### Get notification preferences
```
GET /notifications/preferences?owner=<stellar-address>
→ { "owner": "…", "expiry_warning_enabled": true, "check_in_reminder_enabled": true,
    "vault_released_enabled": true, "warning_hours_before": 24 }
```

### Update notification preferences
```
PUT /notifications/preferences
{ "owner": "…", "expiry_warning_enabled": false, "warning_hours_before": 48 }
```
All fields except `owner` are optional — only provided fields are updated.

### Get delivery log
```
GET /notifications/delivery?owner=<stellar-address>
→ [{ "notification_id": "…", "vault_id": "…", "status": "sent|failed",
     "sent_at": "ISO8601", "provider_response": "projects/…/messages/…" }]
```

## Scheduling

The background scheduler runs every `NOTIFICATION_SCHEDULER_INTERVAL_SECS` seconds and delivers all due pending notifications.

**Expiry warnings** are scheduled automatically when a vault is loaded — they fire `warning_hours_before` hours before the vault's TTL expires.

**Immediate notifications** (vault released, vault paused) are scheduled at the time of the event and delivered on the next scheduler tick.

## Delivery Tracking

Every send attempt (success or failure) is written to the `DeliveryStore` with:
- `status`: `sent` or `failed`
- `provider_response`: FCM message ID on success, error string on failure

Query the delivery log via `GET /notifications/delivery?owner=…`.

## Client Setup

### Android (FCM)
1. Add `google-services.json` to `app/`
2. Implement `FirebaseMessagingService.onNewToken` → call `POST /notifications/register`
3. Handle `onMessageReceived` for foreground messages

### iOS (APNs via FCM)
1. Upload APNs key in Firebase Console → Project Settings → Cloud Messaging
2. Call `UIApplication.registerForRemoteNotifications()` on launch
3. In `AppDelegate.application(_:didRegisterForRemoteNotificationsWithDeviceToken:)` → call `POST /notifications/register`

## Architecture

```
VaultEvent / TTL check
        │
        ▼
NotificationService.schedule_expiry_warning()
NotificationService.schedule_immediate()
        │
        ▼  (every NOTIFICATION_SCHEDULER_INTERVAL_SECS)
NotificationService.flush_pending()
        │
        ├─ check NotificationPreferences (skip if disabled)
        ├─ look up DeviceTokens for owner
        ▼
FcmClient.send() → FCM HTTP v1 API
        │
        ▼
DeliveryRecord written (sent / failed)
```
