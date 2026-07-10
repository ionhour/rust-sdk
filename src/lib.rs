//! # Ionhour Rust SDK
//!
//! A tiny, dependency-lean client for [Ionhour](https://ionhour.com) heartbeat
//! monitoring. Register a **Job** (an inbound heartbeat monitor) with two
//! parameters and send success/fail pings around your scheduled work.
//!
//! 1. `job_token` (required) — the job's ping token. For an unprotected job the
//!    token *is* the credential.
//! 2. `api_key` (optional) — an Ionhour workspace API key (`ionh_…`), only
//!    needed for **protected** jobs. When present it is sent as
//!    `Authorization: Bearer <key>` on every ping; sending it for an
//!    unprotected job is harmless.
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
//! ```no_run
//! use ionhour::{Client, PingOptions};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! // Reads IONHOUR_API_KEY / IONHOUR_BASE_URL from the environment if set.
//! let client = Client::new();
//!
//! // A protected job takes a per-job API key that overrides the client key.
//! let job = client.register_job_with_key("job-token-abc", "ionh_live_xxx")?;
//!
//! // Wrap the work: on Ok pings success with the measured duration; on Err
//! // pings fail with the error message and re-returns your error.
//! let count: u64 = job
//!     .run(|| async { run_backup().await }, PingOptions::new())
//!     .await?;
//! # let _ = count;
//! # Ok(())
//! # }
//! # async fn run_backup() -> Result<u64, std::io::Error> { Ok(42) }
//! ```
//!
//! Manual pings when you don't want the wrapper:
//!
//! ```no_run
//! use ionhour::{Client, PingOptions};
//! use serde_json::json;
//!
//! # async fn demo() -> Result<(), ionhour::Error> {
//! let client = Client::new();
//! let job = client.register_job("job-token-abc")?;
//!
//! // `?` propagates only usage errors (e.g. an oversized payload); a transport
//! // failure comes back as Ok(result) with result.ok == false.
//! let result = job
//!     .success(
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
//! job.fail(PingOptions::new()).await?;
//! # Ok(())
//! # }
//! ```

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
pub use options::{PingOptions, PingResult};
