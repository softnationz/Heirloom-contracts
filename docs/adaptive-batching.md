# Adaptive Batching

Static batch sizes leave throughput on the table: too small and per-batch
overhead dominates, too large and tail latency blows up. `batching::AdaptiveBatcher`
tracks a rolling window of recent batch processing latency and grows or
shrinks the next batch size to track a target latency.

Implementation: `backend/src/batching.rs`.

## Strategy

1. Callers ask `AdaptiveBatcher::current_batch_size()` for how many items to
   process in the next batch.
2. After processing, callers report `record_batch(items, elapsed)`.
3. The batcher pushes `elapsed` into a rolling latency window (size
   `BATCH_LATENCY_WINDOW`) and recomputes the batch size from the window's
   average:
   - **Average > target latency** — shrink proportionally to how far over
     budget the batch was.
   - **Average < 80% of target latency** — grow by up to 25% (capped so a
     single step can't overshoot).
   - Otherwise — hold steady. This slack band avoids oscillating the batch
     size by ±1 on every single batch.
4. The result is always clamped to `[BATCH_MIN_SIZE, BATCH_MAX_SIZE]`.

## Batch size limits

`BatchConfig` bounds the batcher with `min_batch_size`/`max_batch_size`, and
seeds it with `initial_batch_size` before any latency data has been
observed.

## Metrics

Exposed at `GET /metrics` (Prometheus text format):

- `heirloom_protocol_batch_current_size` (gauge)
- `heirloom_protocol_batch_average_latency_ms` (gauge)
- `heirloom_protocol_batches_processed_total` (counter)
- `heirloom_protocol_batch_items_processed_total` (counter)
- `heirloom_protocol_batch_resizes_total` (counter)

## Configuration

| Variable | Default |
|---|---|
| `BATCH_MIN_SIZE` | 1 |
| `BATCH_MAX_SIZE` | 500 |
| `BATCH_INITIAL_SIZE` | 25 |
| `BATCH_TARGET_LATENCY_MS` | 200 |
| `BATCH_LATENCY_WINDOW` | 20 |
