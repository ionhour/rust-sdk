//! The [`Job`] handle: send success/fail pings and wrap fallible work.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::Error;
use crate::options::{PingOptions, PingResult};

/// A registered heartbeat job, bound to a ping token and (optionally) an API
/// key.
///
/// Obtain one from [`Client::register_job`] or
/// [`Client::register_job_with_key`]. Cloning is cheap — the token and key are
/// reference-counted (`Arc<str>`) and the client is `Arc`-backed, so a clone is
/// a few atomic bumps with no string copies.
#[derive(Clone)]
pub struct Job {
    client: Client,
    token: Arc<str>,
    api_key: Option<Arc<str>>,
}

impl Job {
    pub(crate) fn new(client: Client, token: String, api_key: Option<String>) -> Self {
        Self {
            client,
            token: Arc::from(token),
            api_key: api_key.map(Arc::from),
        }
    }

    /// Send a **success** ping.
    ///
    /// Returns `Ok(PingResult)` for reachable outcomes — including
    /// transport/HTTP failures, which live inside the `PingResult` (`ok:
    /// false`) and are also reported to the client's `on_error` hook, never
    /// taking down the host application. Returns `Err` only for a *usage*
    /// mistake: a payload whose serialized size exceeds 10 KiB
    /// ([`Error::PayloadTooLarge`]) or an unserializable payload — no network
    /// call is made in that case.
    pub async fn success(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.client
            .inner()
            .send_ping(&self.token, false, self.api_key.as_deref(), &opts)
            .await
    }

    /// Alias for [`Job::success`].
    pub async fn ping(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.success(opts).await
    }

    /// Send a **failure** ping (the `/fail` variant), marking the monitor down
    /// immediately instead of waiting for the schedule + grace period to lapse.
    ///
    /// Same error contract as [`Job::success`].
    pub async fn fail(&self, opts: PingOptions) -> Result<PingResult, Error> {
        self.client
            .inner()
            .send_ping(&self.token, true, self.api_key.as_deref(), &opts)
            .await
    }

    /// Run a fallible async operation, timing it and reporting the outcome.
    ///
    /// - The closure is invoked and awaited. Wall-clock duration is measured
    ///   with [`Instant`] and sent as the `duration` parameter, overriding any
    ///   `duration_ms` in `opts`.
    /// - On `Ok`, a **success** ping is sent and the value is returned.
    /// - On `Err`, a **failure** ping is sent with `{"error": <message>}`
    ///   merged into `opts.payload`, and the original error is returned
    ///   unchanged — the caller always sees their own failure.
    /// - Every ping error is swallowed here — transport failures (via the
    ///   `on_error` hook) *and* usage errors — so monitoring can never mask or
    ///   replace the operation's own result.
    ///
    /// ```no_run
    /// use ionhour::{Client, PingOptions};
    ///
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = Client::new();
    /// let job = client.register_job("job-token-abc")?;
    ///
    /// let rows: u64 = job
    ///     .run(|| async { do_backup().await }, PingOptions::new())
    ///     .await?;
    /// # let _ = rows;
    /// # Ok(())
    /// # }
    /// # async fn do_backup() -> Result<u64, std::io::Error> { Ok(0) }
    /// ```
    pub async fn run<F, Fut, T, E>(&self, f: F, opts: PingOptions) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
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
                let _ = self.success(ping_opts).await;
                Ok(value)
            }
            Err(error) => {
                let ping_opts = PingOptions {
                    duration_ms: Some(elapsed_ms),
                    payload: Some(merge_error_payload(opts.payload, &error.to_string())),
                    ..opts
                };
                let _ = self.fail(ping_opts).await;
                Err(error)
            }
        }
    }
}

impl fmt::Debug for Job {
    /// Redacts the token and API key — both are credentials and must never
    /// appear in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("token", &"<redacted>")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
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
    fn debug_redacts_token_and_key() {
        let client = Client::builder()
            .base_url("https://example.com/api")
            .build()
            .unwrap();
        let job = client
            .register_job_with_key("super-secret-token", "ionh_secret_key")
            .unwrap();
        let rendered = format!("{job:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "token leaked: {rendered}"
        );
        assert!(
            !rendered.contains("ionh_secret_key"),
            "key leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }
}
