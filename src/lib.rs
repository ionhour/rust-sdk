//! # Ionhour Rust SDK
//!
//! A tiny, dependency-lean client for [Ionhour](https://ionhour.com) heartbeat
//! monitoring. Register a **Job** (an inbound heartbeat monitor) by its ping
//! token and send success/fail pings around your scheduled work. The token
//! *is* the credential: pings send no `Authorization` header.
//!
//! ## Design guarantee
//!
//! Monitoring must never take down the host application. [`Job::success`] and
//! [`Job::fail`] never panic and never `Err` on transport problems — a
//! transport error or non-2xx status is carried in the returned
//! `Ok(`[`PingResult`]`)` (`ok: false`) and reported to the optional `on_error`
//! hook. Only *usage* mistakes are loud: an empty/whitespace token fails at
//! registration, and an oversized (> 10 KiB) or unserializable payload is
//! returned as an [`Error`] from the ping call before any network request. This
//! mirrors the sibling SDKs, where usage errors are surfaced and transport
//! errors stay silent.
//!
//! ## Quickstart
//!
//! The raw ping is a single HTTP GET — this is the exact `curl` equivalent:
//!
//! ```sh
//! curl -m 10 --retry 2 https://signal.ionhour.com/api/signals/ping/your-job-token
//! ```
//!
//! The SDK is that same ping with retries, timeouts, and failure signaling
//! handled for you. The shortest path — no config, no options — is two lines:
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let job = ionhour::Client::new().register_job("your-job-token")?;
//! job.success().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Wrap a fallible task with [`Job::run`] to send a start ping, then ping
//! success on `Ok` (with the measured duration) or fail on `Err`, re-returning
//! your own error. The start and terminal pings share an auto-generated `rid`
//! so they correlate:
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let job = ionhour::Client::new().register_job("your-job-token")?;
//! let count: u64 = job.run(|| async { run_backup().await }).await?;
//! # let _ = count;
//! # Ok(())
//! # }
//! # async fn run_backup() -> Result<u64, std::io::Error> { Ok(42) }
//! ```
//!
//! Configure explicitly with [`Client::builder`], and attach telemetry with the
//! `_with` variants — [`Job::success_with`], [`Job::fail_with`],
//! [`Job::run_with`]:
//!
//! ```no_run
//! use ionhour::{Client, PingOptions};
//! use serde_json::json;
//!
//! # async fn demo() -> Result<(), ionhour::Error> {
//! // Reads IONHOUR_BASE_URL from the environment if set.
//! let client = Client::builder().timeout(std::time::Duration::from_secs(5)).build()?;
//! let job = client.register_job("your-job-token")?;
//!
//! // `?` propagates only usage errors (e.g. an oversized payload); a transport
//! // failure comes back as Ok(result) with result.ok == false.
//! let result = job
//!     .success_with(
//!         PingOptions::new()
//!             .rid("run-42")
//!             .duration_ms(1200)
//!             .payload(json!({ "records": 10 })),
//!     )
//!     .await?;
//!
//! if !result.ok {
//!     // Inspect result.error / result.status; the process keeps running.
//! }
//!
//! // Report an explicit failure so the monitor goes down immediately.
//! job.fail().await?;
//! # Ok(())
//! # }
//! ```
//!
//! [`PingOptions`] also carries telemetry — `host`, `env`, and `exit_code`
//! (plus `rid`/`duration`) — mapped to query parameters the server records.
//! [`Job::start`] sends a standalone start ping (persist-only run telemetry
//! that never changes monitor status); [`Job::run`] emits one automatically.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod client;
mod error;
mod job;
mod options;

pub use client::Client;
pub use client::{
    ClientBuilder, DEFAULT_BASE_URL, DEFAULT_RETRIES, DEFAULT_TIMEOUT, MAX_PAYLOAD_BYTES,
};
pub use error::Error;
pub use job::Job;
pub use options::{PingContext, PingOptions, PingResult};
