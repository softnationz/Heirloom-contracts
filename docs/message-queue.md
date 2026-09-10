# Message Queue Integration (#150)

The message queue layer (`backend/src/message_queue.rs`) provides Kafka/RabbitMQ-style
async messaging without requiring an external broker. A `MessageBroker` trait boundary
makes it straightforward to swap in a real Kafka or AMQP client for production.

## Architecture overview

```
Publisher  ──publish──►  MessageBroker  ──poll──►  Consumer
                              │
                         TopicStore
                        ┌────┴────┐
                        │ partition 0
                        │ partition 1
                        │ partition 2  ← QueueMessage[]
                        │ partition 3
                        └─────────┘
                              │
                             DLQ  (dead-letter queue)
```

## Topics

Topics are registered before use. Each topic has its own partition count, routing
strategy, and retry limit.

```rust
broker.create_topic(
    TopicConfig::new("vault.events")
        .with_partitions(4)
        .with_strategy(PartitionStrategy::HashKey)
        .with_max_retries(3),
)?;
```

The default topic `"vault.events"` is created automatically when `MessageQueueState::new()`
is called during server startup.

## Partitioning strategies

| Strategy | Behaviour |
|---|---|
| `RoundRobin` | Messages distributed evenly, one per partition in order |
| `HashKey` | Message key is hashed (djb2) to a stable partition — same key always lands in the same partition |
| `Manual` | Caller supplies the target partition number explicitly |

`HashKey` is the default for `vault.events`. This ensures all events for a given
`vault_id` are ordered within a single partition, which is important for consumers
that rebuild vault state.

## Publishing

### Via `MessagePublisher` (recommended)

```rust
// Injected from AppState
state.message_queue.publisher.publish_vault_event(
    "vault-abc",
    "check_in",
    serde_json::json!({"ttl_remaining": 86400}),
)?;
```

The publisher automatically sets `event_type` and `source` headers.

### Via `MessageBroker` directly

```rust
let mut headers = HashMap::new();
headers.insert("schema_version".into(), "1".into());

broker.publish(
    "vault.events",
    Some("vault-abc".into()),  // routing key
    headers,
    serde_json::json!({"balance_delta": 500}),
    None,  // partition assigned automatically
)?;
```

## Consuming

### Build a consumer

```rust
// Listen to all 4 partitions of "vault.events" as consumer group "notifier"
let consumer = state.message_queue.consumer("notifier", "vault.events", 4);
```

### Poll and process manually

```rust
let messages = consumer.poll_all(50)?;  // up to 50 per partition
for msg in &messages {
    // process …
}
// Acknowledge offset 50 on partition 0
consumer.ack("vault.events", 0, 50)?;
```

### Process with auto-ack and DLQ

```rust
let summary = consumer.process(
    "vault.events",
    0,          // partition
    100,        // max messages to fetch
    3,          // max retries before DLQ
    |msg| {
        // return Ok(()) on success, Err(reason) to retry
        handle_event(msg).map_err(|e| e.to_string())
    },
)?;
println!("processed={} failed={} dlq={}", summary.processed, summary.failed, summary.dlq_count);
```

Semantics:
- On `Ok(())`: message offset is advanced (acknowledged).
- On `Err(...)`: if `delivery_attempts >= max_retries`, the message is moved to the DLQ
  and the offset advances. Otherwise, processing stops and the message is retried on the
  next poll.

## Dead-letter queue (DLQ)

Messages that exceed `max_retries` are moved to the topic's DLQ automatically. Inspect
them with:

```rust
let dead = broker.list_dlq("vault.events")?;
for msg in dead {
    println!("id={} reason={}", msg.id, msg.headers["dlq_reason"]);
}
```

## Message envelope

Every `QueueMessage` carries:

| Field | Type | Description |
|---|---|---|
| `id` | `String` (UUID v4) | Unique message identifier |
| `topic` | `String` | Target topic name |
| `partition` | `u32` | Assigned partition |
| `key` | `Option<String>` | Routing / correlation key |
| `headers` | `HashMap<String,String>` | Application metadata |
| `payload` | `serde_json::Value` | Message body |
| `published_at` | `DateTime<Utc>` | Publish timestamp |
| `delivery_attempts` | `u32` | Incremented on each failed delivery |

## Integration with `AppState`

`MessageQueueState` is injected as `AppState.message_queue`:

```rust
// From any Axum handler:
state.message_queue.publisher.publish_vault_event(vault_id, "deposit", payload)?;
```

## Swapping in a real broker

Implement the same publish/poll/commit_offset semantics against a Kafka or RabbitMQ
client and replace `MessageBroker` in `MessageQueueState::new()`. The `MessagePublisher`
and `MessageConsumer` wrappers do not need to change.

## Running the tests

```bash
cargo test -p heirloom-protocol-backend message_queue
```

All tests live in the `tests` module at the bottom of `message_queue.rs`.
