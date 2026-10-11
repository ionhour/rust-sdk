# ionhour

Rust SDK for [Ionhour](https://ionhour.com) heartbeat monitoring. Register a
**Job** (an inbound heartbeat monitor) and send success/fail pings around your
scheduled work — cron jobs, backups, ETL, queue workers.

Small and dependency-lean: `reqwest` (rustls), `serde`, `tokio`, `thiserror`.

## Install

```sh
cargo add ionhour
```

## Quickstart

A ping is a single HTTP GET. This is the raw request — no SDK required:

```sh
curl -m 10 --retry 2 https://signal.ionhour.com/api/signals/ping/your-job-token
```

The SDK is the **same ping with retries, timeouts, and failure signaling
handled for you**. The shortest path is two lines — the base URL comes from
`IONHOUR_BASE_URL` when not passed explicitly:

```rust
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let job = ionhour::Client::new().register_job("your-job-token")?;
job.success().await?;
# Ok(())
# }
```

## Slug mode

Address a job by a human-readable slug in your API key's workspace instead of
by token, and optionally create it on the first ping. The curl equivalent:

```sh
curl -H "Authorization: Bearer $IONHOUR_API_KEY" \
  "https://api.ionhour.com/api/signals/s/nightly-report?create=1"
```

```rust
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
use ionhour::{Client, SlugJobOptions};

// Reads IONHOUR_API_KEY (and IONHOUR_API_BASE_URL, default https://api.ionhour.com/api).
let job = Client::new()
    .register_job_by_slug_with("nightly-report", SlugJobOptions::new().create(true))?;
let result = job.success().await?;
let _ = result.created; // true on the ping that created the job
# Ok(())
# }
```

`register_job_by_slug(slug)` is the form without auto-creation. The returned
`Job` has the same `success` / `fail` / `start` / `run` surface. Slug pings go
to the **API host** with `Authorization: Bearer <key>`; token pings never send
the key. Registration returns `Error::InvalidSlug` for a slug outside
`^[a-z0-9_-]{1,64}$` (it keeps only a short prefix of the rejected value), `Error::MissingApiKey` when no key is configured, and
`Error::InvalidApiKey` for a key with whitespace or control characters.
Auto-creation needs the server flag `job_auto_provisioning`, which is OFF by
default; with it off a create ping comes back as
`Error::Http { status: 403, code: Some("FEATURE_DISABLED"), .. }`. A malformed
`IONHOUR_API_BASE_URL` surfaces as `Error::InvalidConfig` from slug
registration and never affects token pings. Slug 4xx
responses are never retried. 429 and 5xx use the usual backoff. The
`on_error` hook's `PingContext::slug()` names the slug job.

## The `run` wrapper

`run` wraps a fallible task — it times the closure with a monotonic clock and
reports the outcome for you, re-returning your own error untouched:

```rust
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let job = ionhour::Client::new().register_job("your-job-token")?;

let rows: u64 = job.run(|| async { run_backup().await }).await?;
# let _ = rows;
# Ok(())
# }
# async fn run_backup() -> Result<u64, std::io::Error> { Ok(42) }
```

- Before the closure runs, it sends a best-effort **start** ping so you can see
  the run in flight. Unless you supplied a `rid`, one is generated and shared by
  the start and terminal pings so they correlate.
- On `Ok`, it sends a **success** ping with the measured `duration`.
- On `Err`, it sends a **failure** ping with `{"error": "<message>"}` merged
  into your payload, then returns the original error unchanged.
- Ping transport failures never mask your result — they only reach the
  `on_error` hook. The start ping can be disabled with
  `PingOptions::new().start_ping(false)`.

## Telemetry and the `_with` variants

A job is registered by its `job_token`, which is the job's credential. Token pings
send no `Authorization` header.

The bare `success()` / `ping()` / `fail()` / `start()` / `run(fn)` calls take no
arguments. When you need a run id, duration, telemetry, or a payload, use the
`_with` variants — `success_with` / `ping_with` / `fail_with` / `start_with` /
`run_with`. `PingOptions` is `#[non_exhaustive]`; build it with
`PingOptions::new()` and the chainable setters: `rid`, `duration_ms`, `host`
(truncated to 253 chars), `env` (truncated to 32 chars), `exit_code` (a `u8`, so
0–255 is a compile-time guarantee), and `payload`.

`start()` / `start_with()` send a **start** ping to `/signals/ping/{token}/start`
— persist-only run telemetry that records a run beginning but never changes
monitor status, opens/resolves incidents, or resets the schedule. Pair it with a
terminal ping via a shared `rid` (which `run` does for you).

```rust
use ionhour::{Client, PingOptions};
use serde_json::json;

# async fn demo() -> Result<(), ionhour::Error> {
let client = Client::new();

let job = client.register_job("your-job-token")?;

// Success with metadata. `?` propagates only usage errors (e.g. an oversized
// payload); a transport failure comes back as Ok(result) with result.ok == false.
let result = job
    .success_with(
        PingOptions::new()
            .rid("run-42")
            .duration_ms(1200)
            .host("db-1.internal")
            .env("production")
            .payload(json!({ "records": 10 })),
    )
    .await?;

if !result.ok {
    // Inspect result.error / result.status. The process keeps running —
    // a monitoring outage never takes down your application.
    eprintln!("ping failed: {:?}", result.error);
}

// Explicit failure ping: marks the monitor down immediately.
job.fail().await?;
# Ok(())
# }
```

## Error philosophy

Monitoring must never take down the host application. The rule matches the
sibling SDKs — **usage errors are loud, transport errors are silent**:

- `success()` / `fail()` return `Result<PingResult, Error>`. A transport failure
  or non-2xx status is **not** an `Err`: it comes back as
  `Ok(PingResult { ok: false, status, error })` and is also passed to the
  optional `on_error` hook, so a monitoring outage never propagates into your
  code.
- `Err` is reserved for **usage** mistakes only: a payload whose serialized size
  exceeds 10 KiB (`Error::PayloadTooLarge`) or an unserializable payload — in
  both cases no network call is made. An empty/whitespace token fails even
  earlier, at `register_job` with `Error::InvalidToken`.
- `run(fn)` **re-returns** `fn`'s error after sending the fail ping, and
  swallows *all* of its own ping errors (transport and usage) so it can never
  change your operation's result.

## Configuration

Configure via `Client::builder()`. Precedence is **explicit value → environment
variable → default**.

| Option       | Builder method | Env override        | Default                            |
|--------------|----------------|---------------------|------------------------------------|
| Base URL     | `base_url`     | `IONHOUR_BASE_URL`  | `https://signal.ionhour.com/api`   |
| Timeout      | `timeout`      | —                   | 10s per attempt                    |
| Retries      | `retries`      | —                   | 3 attempts (backoff 1s, then 2s clamped) |
| User-Agent   | `user_agent`   | —                   | `ionhour-rust/<version>`           |
| Error hook   | `on_error`     | —                   | none                               |
| API base URL | `api_base_url` | `IONHOUR_API_BASE_URL` | `https://api.ionhour.com/api` (slug mode only) |
| API key      | `api_key`      | `IONHOUR_API_KEY`   | none (slug mode only; trimmed, blank = absent, redacted in `Debug`) |

```rust
use std::time::Duration;
use ionhour::Client;

# fn demo() -> Result<(), ionhour::Error> {
let client = Client::builder()
    .timeout(Duration::from_secs(5))
    .retries(4)
    .on_error(|err, ctx| {
        eprintln!(
            "ionhour ping failed for {} (fail_ping={}): {err}",
            ctx.token(),
            ctx.is_fail(),
        )
    })
    .build()?;
# let _ = client;
# Ok(())
# }
```

The `on_error` hook fires only for silent (transport / HTTP) failures; usage
errors are returned to you directly and do not reach it. It receives a
`PingContext` alongside the error — the job token and whether the failing ping
was a `fail` ping — so a single hook shared across many jobs can attribute each
failure.

Retries apply only to network errors, HTTP 429, and 5xx responses; other 4xx
responses are never retried. `Client` is cheaply cloneable (`Arc` inside) —
build one and share it across your application.

> **Note:** token jobs authenticate with the ping token in the URL alone and
> never send an `Authorization` header, even when `api_key` or
> `IONHOUR_API_KEY` is set. The key is sent only by slug-mode jobs.

## Docs

## Base URL

The default `base_url` is `https://signal.ionhour.com/api`, the dedicated
heartbeat-ingest host. Set `IONHOUR_BASE_URL=https://api.ionhour.com/api` (or
`ClientBuilder::base_url`) to send pings through the main API host instead; it
serves every ping route (`/ping/{token}`, `/fail`, `/start`) and is the right
choice when you also use failure or start pings and your deployment has not yet
enabled them on the signal host.

Full documentation: <https://docs.ionhour.com> · API reference on
[docs.rs](https://docs.rs/ionhour) · machine-readable index for AI agents:
<https://ionhour.com/llms.txt>.

## License

MIT — see [LICENSE](LICENSE).
