//! The [`Job`] handle: send success/fail pings and wrap fallible work.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::client::{Client, PingKind};
use crate::error::Error;
use crate::options::{PingOptions, PingResult};

/// Process-wide monotonic counter that disambiguates auto-generated run ids
/// produced within the same nanosecond tick. See [`generate_rid`].
static RID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A registered heartbeat job, bound to a ping token.
///
/// Obtain one from [`Client::register_job`]. Cloning is cheap — the token is
/// reference-counted (`Arc<str>`) and the client is `Arc`-backed, so a clone is
/// a few atomic bumps with no string copies.
#[derive(Clone)]
pub struct Job {
    client: Client,
    target: JobTarget,
}

/// What a job pings: a token on the signal host (no credential), or a slug on
/// the API host (Bearer API key from the client).
#[derive(Clone)]
pub(crate) enum JobTarget {
    Token(Arc<str>),
    Slug { slug: Arc<str>, create: bool },
}

impl Job {
    pub(crate) fn new(client: Client, token: String) -> Self {
        Self {
            client,
            target: JobTarget::Token(Arc::from(token)),
        }
    }

    pub(crate) fn new_slug(client: Client, slug: String, create: bool) -> Self {
        Self {
            client,
            target: JobTarget::Slug {
                slug: Arc::from(slug),
                create,
            },
        }
    }

    /// Send a **success** ping with no telemetry — the zero-ceremony form.
    ///
    /// This is the shortest path: `job.success().await?`. It is exactly
    /// [`Job::success_with`] called with [`PingOptions::default`]; reach for the
    /// `_with` form only when you need a `rid`, duration, or payload.
    ///
    /// Same return contract as [`Job::success_with`].
    pub async fn success(&self) -> Result<PingResult, Error> {
        self.success_with(PingOptions::default()).await
    }

    /// Send a **success** ping carrying [`PingOptions`] (run id, duration,
    /// payload).
    ///
    /// Returns `Ok(PingResult)` for reachable outcomes — including
    /// transport/HTTP failures, which live inside the `PingResult` (`ok:
    /// false`) and are also reported to the client's `on_error` hook, never
    /// taking down the host application. Returns `Err` only for a *usage*
    /// mistake: a payload whose serialized size exceeds 10 KiB
    /// ([`Error::PayloadTooLarge`]) or an unserializable payload — no network
    /// call is made in that case.
    pub async fn success_with(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.client
            .inner()
            .send_ping(&self.target, PingKind::Success, &opts)
            .await
    }

    /// Alias for [`Job::success`] — a bare success ping with no telemetry.
    pub async fn ping(&self) -> Result<PingResult, Error> {
        self.success().await
    }

    /// Alias for [`Job::success_with`] — a success ping carrying [`PingOptions`].
    pub async fn ping_with(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.success_with(opts).await
    }

    /// Send a **failure** ping with no telemetry — the zero-ceremony form.
    ///
    /// Marks the monitor down immediately instead of waiting for the schedule +
    /// grace period to lapse. Equivalent to [`Job::fail_with`] with
    /// [`PingOptions::default`].
    pub async fn fail(&self) -> Result<PingResult, Error> {
        self.fail_with(PingOptions::default()).await
    }

    /// Send a **failure** ping (the `/fail` variant) carrying [`PingOptions`],
    /// marking the monitor down immediately instead of waiting for the
    /// schedule + grace period to lapse.
    ///
    /// Same error contract as [`Job::success_with`].
    pub async fn fail_with(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.client
            .inner()
            .send_ping(&self.target, PingKind::Fail, &opts)
            .await
    }

    /// Send a **start** ping with no telemetry — the zero-ceremony form.
    ///
    /// A start ping is persist-only run telemetry: it records that a run began
    /// but never changes monitor status, opens/resolves incidents, or resets the
    /// schedule — a job that starts and then hangs still goes LATE. Pair it with
    /// the terminal ping via a shared `rid` (which [`Job::run`] does for you).
    /// Equivalent to [`Job::start_with`] with [`PingOptions::default`].
    pub async fn start(&self) -> Result<PingResult, Error> {
        self.start_with(PingOptions::default()).await
    }

    /// Send a **start** ping (the `/start` variant) carrying [`PingOptions`].
    ///
    /// Same error contract as [`Job::success_with`], including the URL scrubbing
    /// on transport failures. A `payload` promotes the ping to a `POST`, exactly
    /// like the success/fail pings.
    pub async fn start_with(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.client
            .inner()
            .send_ping(&self.target, PingKind::Start, &opts)
            .await
    }

    /// Run a fallible async operation, timing it and reporting the outcome — the
    /// zero-ceremony form.
    ///
    /// Equivalent to [`Job::run_with`] with [`PingOptions::default`]; use the
    /// `_with` form when you want to attach a `rid` or a base payload to the
    /// automatic pings.
    ///
    /// ```no_run
    /// use ionhour::Client;
    ///
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::new();
    /// let job = client.register_job("your-job-token")?;
    ///
    /// let rows: u64 = job.run(|| async { do_backup().await }).await?;
    /// # let _ = rows;
    /// # Ok(())
    /// # }
    /// # async fn do_backup() -> Result<u64, std::io::Error> { Ok(0) }
    /// ```
    pub async fn run<F, Fut, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        self.run_with(f, PingOptions::default()).await
    }

    /// Run a fallible async operation, timing it and reporting the outcome,
    /// seeding the automatic pings with `opts`.
    ///
    /// - Unless `opts.rid` is set, a run id is generated so the start and
    ///   terminal pings correlate; a caller-supplied `rid` is preserved.
    /// - Unless `opts.start_ping` is `false`, a best-effort **start** ping is
    ///   sent before the closure runs, carrying `rid`/`host`/`env` only.
    /// - The closure is invoked and awaited. Wall-clock duration is measured
    ///   with [`Instant`] and sent as the `duration` parameter, overriding any
    ///   `duration_ms` in `opts`.
    /// - On `Ok`, a **success** ping is sent and the value is returned.
    /// - On `Err`, a **failure** ping is sent with `{"error": <message>}`
    ///   merged into `opts.payload`, and the original error is returned
    ///   unchanged — the caller always sees their own failure.
    /// - Every ping error is swallowed here — the start ping, transport
    ///   failures (via the `on_error` hook), *and* usage errors — so monitoring
    ///   can never mask or replace the operation's own result.
    ///
    /// ```no_run
    /// use ionhour::{Client, PingOptions};
    ///
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::new();
    /// let job = client.register_job("your-job-token")?;
    ///
    /// let rows: u64 = job
    ///     .run_with(
    ///         || async { do_backup().await },
    ///         PingOptions::new().host("db-1").env("production"),
    ///     )
    ///     .await?;
    /// # let _ = rows;
    /// # Ok(())
    /// # }
    /// # async fn do_backup() -> Result<u64, std::io::Error> { Ok(0) }
    /// ```
    pub async fn run_with<F, Fut, T, E>(&self, f: F, opts: PingOptions) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        // Ensure a rid so the start and terminal pings correlate. A
        // caller-supplied rid is preserved; otherwise generate one.
        let opts = if opts.rid.is_some() {
            opts
        } else {
            PingOptions {
                rid: Some(generate_rid()),
                ..opts
            }
        };

        // Best-effort start ping (persist-only telemetry) carrying rid/host/env
        // only — no duration/exit_code/payload, which aren't known yet. Its
        // error is swallowed like every other internal ping.
        if opts.start_ping {
            let start_opts = PingOptions {
                rid: opts.rid.clone(),
                host: opts.host.clone(),
                env: opts.env.clone(),
                ..PingOptions::default()
            };
            let _ = self.start_with(start_opts).await;
        }

        let start = Instant::now();
        let outcome = f().await;
        let elapsed_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        match outcome {
            Ok(value) => {
                let ping_opts = PingOptions {
                    duration_ms: Some(elapsed_ms),
                    ..opts
                };
                // Swallow every ping error (transport and usage) — run() must
                // never let a monitoring failure change the operation's result.
                let _ = self.success_with(ping_opts).await;
                Ok(value)
            }
            Err(error) => {
                let ping_opts = PingOptions {
                    duration_ms: Some(elapsed_ms),
                    payload: Some(merge_error_payload(opts.payload, &error.to_string())),
                    ..opts
                };
                let _ = self.fail_with(ping_opts).await;
                Err(error)
            }
        }
    }
}

/// Generate a dependency-free run-correlation id: the current wall-clock time in
/// nanoseconds combined with a process-wide counter, so two runs starting in the
/// same nanosecond tick still get distinct ids. The result fits comfortably in
/// the server's 64-character `rid` limit.
fn generate_rid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = RID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("r{nanos:x}-{count:x}")
}

impl fmt::Debug for Job {
    /// Redacts the token — it is the credential and must never appear in logs.
    /// A slug is not a credential and is shown; the API key never is.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.target {
            JobTarget::Token(_) => f
                .debug_struct("Job")
                .field("token", &"<redacted>")
                .finish_non_exhaustive(),
            JobTarget::Slug { slug, create } => f
                .debug_struct("Job")
                .field("slug", &&**slug)
                .field("create", create)
                .finish_non_exhaustive(),
        }
    }
}

/// Merge `{"error": <message>}` into an existing payload.
///
/// - `None` → `{"error": message}`.
/// - An object → a clone with the `error` key inserted (overwriting any
///   existing `error`).
/// - Any other JSON value → `{"error": message, "payload": <original>}` so the
///   caller's data is preserved without being silently dropped.
fn merge_error_payload(existing: Option<Value>, message: &str) -> Value {
    match existing {
        None => json!({ "error": message }),
        Some(Value::Object(mut map)) => {
            map.insert("error".to_string(), Value::String(message.to_string()));
            Value::Object(map)
        }
        Some(other) => json!({ "error": message, "payload": other }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_none_creates_object() {
        assert_eq!(
            merge_error_payload(None, "boom"),
            json!({ "error": "boom" })
        );
    }

    #[test]
    fn merge_into_object_inserts_key() {
        let existing = json!({ "count": 3 });
        assert_eq!(
            merge_error_payload(Some(existing), "boom"),
            json!({ "count": 3, "error": "boom" })
        );
    }

    #[test]
    fn merge_overwrites_existing_error_key() {
        let existing = json!({ "error": "old" });
        assert_eq!(
            merge_error_payload(Some(existing), "new"),
            json!({ "error": "new" })
        );
    }

    #[test]
    fn merge_into_non_object_wraps_under_payload() {
        let existing = json!([1, 2, 3]);
        assert_eq!(
            merge_error_payload(Some(existing), "boom"),
            json!({ "error": "boom", "payload": [1, 2, 3] })
        );
    }

    #[test]
    fn generated_rids_are_distinct_and_within_limit() {
        let a = generate_rid();
        let b = generate_rid();
        assert_ne!(a, b, "the counter must disambiguate same-tick ids");
        for rid in [&a, &b] {
            assert!(rid.starts_with('r'));
            assert!(rid.contains('-'));
            assert!(
                rid.len() <= 64,
                "rid must fit the server's 64-char limit: {rid}"
            );
        }
    }

    #[test]
    fn debug_redacts_token() {
        let client = Client::builder()
            .base_url("https://example.com/api")
            .build()
            .unwrap();
        let job = client.register_job("super-secret-token").unwrap();
        let rendered = format!("{job:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "token leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }
}
