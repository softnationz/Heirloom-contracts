# Predictive Scaling

Reactive autoscaling only adds capacity after load has already spiked, so
users feel the lag while new capacity comes online. `predictive_scaling::PredictiveScaler`
keeps a rolling history of traffic samples, forecasts near-term demand, and
recommends a replica count ahead of time.

Implementation: `backend/src/predictive_scaling.rs`.

## Traffic history

A background task (`predictive_scaling::run`, spawned from `main.rs`)
samples the delta of `heirloom_protocol_http_requests_total` every
`sample_interval` (5 minutes in production) and records it as a
`TrafficSample` in `PredictiveScaler::history` (default: 288 samples, i.e.
24h of 5-minute buckets).

## Forecasting model

`ForecastModel` implements **Holt's double exponential smoothing**: it
tracks a smoothed *level* and *trend* across the sample history so the
forecast accounts for traffic that is actively rising or falling, not just
its most recent value. `alpha`/`beta` control how quickly the level/trend
adapt to new samples.

## Predictive scaling algorithm

On each evaluation:

1. Forecast demand `SCALING_FORECAST_PERIODS_AHEAD` sampling intervals into
   the future.
2. Convert the forecast to a replica count: `ceil(forecast / SCALING_REQUESTS_PER_REPLICA)`.
3. Clamp to `[SCALING_MIN_REPLICAS, SCALING_MAX_REPLICAS]`.
4. If the recommendation changed, apply it via `AutoscalerClient`.

## Autoscaling integration

`AutoscalerClient` is a small trait (`set_desired_replicas(replicas: u32)`)
so `PredictiveScaler` stays decoupled from any specific platform. The
default `LoggingAutoscalerClient` just logs the recommendation — swap it for
a real Kubernetes HPA / ECS service-scaling client to actually drive
infrastructure.

## Metrics

Exposed at `GET /metrics` (Prometheus text format):

- `heirloom_protocol_scaling_recommended_replicas` (gauge)
- `heirloom_protocol_scaling_forecast_requests` (gauge)
- `heirloom_protocol_scaling_decisions_total` (counter)

## Configuration

| Variable | Default |
|---|---|
| `SCALING_MIN_REPLICAS` | 2 |
| `SCALING_MAX_REPLICAS` | 50 |
| `SCALING_REQUESTS_PER_REPLICA` | 100 |
| `SCALING_FORECAST_PERIODS_AHEAD` | 3 |
