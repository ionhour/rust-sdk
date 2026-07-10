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

A job is registered with **two parameters**: the required `job_token` (the
job's ping token) and an optional `api_key`. The API key is only needed for a
**protected** job; when present it is sent as `Authorization: Bearer <key>` on
every ping. Sending it for an unprotected job is harmless, so it is always safe
to pass one.

```rust
use ionhour::{Client, PingOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads IONHOUR_API_KEY / IONHOUR_BASE_URL from the environment if set.
    let client = Client::new();

    // Protected job: the per-job key overrides any client-wide key.
    let job = client.register_job_with_key("job-token-abc", "ionh_live_xxx")?;

    // For an unprotected job, omit the key:
    // let job = client.register_job("job-token-abc")?;

    // Wrap the work: pings success with the measured duration on Ok, or fail
    // (with the error message) on Err — and re-returns your original error.
    let rows: u64 = job
        .run(|| async { run_backup().await }, PingOptions::new())
        .await?;

    println!("backed up {rows} rows");
    Ok(())
}

async fn run_backup() -> Result<u64, std::io::Error> {
    Ok(42)
}
```

## The `run` wrapper

`run` times the closure with a monotonic clock and reports the outcome for you:

- On `Ok`, it sends a **success** ping with the measured `duration`.
- On `Err`, it sends a **failure** ping with `{"error": "<message>"}` merged
  into your payload, then returns the original error unchanged.
- Ping transport failures never mask your result — they only reach the
  `on_error` hook.

## Manual pings

`PingOptions` is `#[non_exhaustive]`; build it with `PingOptions::new()` and the
chainable setters.

```rust
use ionhour::{Client, PingOptions};
use serde_json::json;

# async fn demo() -> Result<(), ionhour::Error> {
let client = Client::new();
let job = client.register_job("job-token-abc")?;

// Success with metadata. `?` propagates only usage errors (e.g. an oversized
// payload); a transport failure comes back as Ok(result) with result.ok == false.
let result = job
    .success(
        PingOptions::new()
            .rid("run-42")
            .duration_ms(1200)
            .payload(json!({ "records": 10 })),
    )
    .await?;

if !result.ok {
    // Inspect result.error / result.status. The process keeps running —
    // a monitoring outage never takes down your application.
    eprintln!("ping failed: {:?}", result.error);
}

// Explicit failure ping: marks the monitor down immediately.
job.fail(PingOptions::new()).await?;
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
| API key      | `api_key`      | `IONHOUR_API_KEY`   | none (only needed for protected jobs) |
| Timeout      | `timeout`      | —                   | 10s per attempt                    |
| Retries      | `retries`      | —                   | 3 attempts (backoff 1s, 2s)        |
| User-Agent   | `user_agent`   | —                   | `ionhour-rust/<version>`           |
| Error hook   | `on_error`     | —                   | none                               |

```rust
use std::time::Duration;
use ionhour::Client;

# fn demo() -> Result<(), ionhour::Error> {
let client = Client::builder()
    .api_key("ionh_live_xxx")
    .timeout(Duration::from_secs(5))
    .retries(4)
    .on_error(|err| eprintln!("ionhour ping failed: {err}"))
    .build()?;
# let _ = client;
# Ok(())
# }
```

The `on_error` hook fires only for silent (transport / HTTP) failures; usage
errors are returned to you directly and do not reach it.

Retries apply only to network errors, HTTP 429, and 5xx responses; other 4xx
responses are never retried. `Client` is cheaply cloneable (`Arc` inside) —
build one and share it across your application.

> **Note:** the API key is only required for **protected** jobs. Unprotected
> jobs authenticate with the ping token in the URL alone.

## Docs

Full documentation: <https://docs.ionhour.com> · API reference on
[docs.rs](https://docs.rs/ionhour).

## License

MIT — see [LICENSE](LICENSE).
