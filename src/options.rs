//! Per-ping options and the result type returned by ping methods.

use std::fmt;
use std::sync::Arc;

use crate::error::Error;

/// Optional metadata attached to a single ping.
///
/// This struct is `#[non_exhaustive]`, so construct it with [`PingOptions::new`]
/// (or [`PingOptions::default`]) and the chainable setters rather than a struct
/// literal:
///
/// ```
/// use ionhour::PingOptions;
/// use serde_json::json;
///
/// let opts = PingOptions::new()
///     .rid("run-42")
///     .duration_ms(1234)
///     .host("db-1.internal")
///     .env("production")
///     .exit_code(0)
///     .payload(json!({ "records": 10 }));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PingOptions {
    /// Run correlation id, sent as the `rid` query parameter. Use the same
    /// `rid` on the start and end pings of one logical run to correlate them.
    pub rid: Option<String>,

    /// Execution time in milliseconds, sent as the `duration` query parameter.
    ///
    /// When you use [`crate::Job::run`], this is populated automatically from
    /// the measured wall-clock time and overrides any value you set here.
    pub duration_ms: Option<u64>,

    /// Arbitrary JSON payload sent as the request body.
    ///
    /// When present the ping is sent as a `POST` with a JSON body; when absent
    /// it is a `GET`. The serialized body must be at most 10 KiB or the ping is
    /// rejected client-side with [`Error::PayloadTooLarge`].
    pub payload: Option<serde_json::Value>,

    /// Reporting host name, sent as the `host` query parameter. Set it
    /// explicitly — it is never auto-detected. Values longer than 253
    /// characters are silently truncated (the server's limit).
    pub host: Option<String>,

    /// Deployment environment (for example `"production"`), sent as the `env`
    /// query parameter. Set it explicitly — never auto-detected. Values longer
    /// than 32 characters are silently truncated (the server's limit).
    pub env: Option<String>,

    /// Process exit code, sent as the `exit_code` query parameter. The `u8`
    /// type makes the 0–255 range a compile-time guarantee, so no runtime
    /// validation is needed.
    pub exit_code: Option<u8>,

    /// Whether [`crate::Job::run`] / [`crate::Job::run_with`] send a best-effort
    /// **start** ping before invoking the wrapped closure. Defaults to `true`.
    ///
    /// This flag is **run-only**: the direct pings ([`crate::Job::success`],
    /// [`crate::Job::fail`], [`crate::Job::start`] and their `_with` forms)
    /// never consult it.
    pub start_ping: bool,
}

impl Default for PingOptions {
    fn default() -> Self {
        Self {
            rid: None,
            duration_ms: None,
            payload: None,
            host: None,
            env: None,
            exit_code: None,
            // Run wrappers emit a start ping unless the caller opts out.
            start_ping: true,
        }
    }
}

impl PingOptions {
    /// Create an empty set of options (all fields unset, `start_ping` on).
    /// Equivalent to [`PingOptions::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the run correlation id (`rid` query parameter).
    pub fn rid(mut self, rid: impl Into<String>) -> Self {
        self.rid = Some(rid.into());
        self
    }

    /// Set the execution time in milliseconds (`duration` query parameter).
    pub fn duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Set the JSON payload sent as the request body.
    pub fn payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Set the reporting host (`host` query parameter), silently truncated to
    /// 253 characters (on a UTF-8 char boundary) to match the server's limit.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(truncate_chars(host.into(), MAX_HOST_CHARS));
        self
    }

    /// Set the deployment environment (`env` query parameter), silently
    /// truncated to 32 characters (on a UTF-8 char boundary) to match the
    /// server's limit.
    pub fn env(mut self, env: impl Into<String>) -> Self {
        self.env = Some(truncate_chars(env.into(), MAX_ENV_CHARS));
        self
    }

    /// Set the process exit code (`exit_code` query parameter). The 0–255 range
    /// is enforced by the `u8` type.
    pub fn exit_code(mut self, exit_code: u8) -> Self {
        self.exit_code = Some(exit_code);
        self
    }

    /// Enable or disable the automatic **start** ping sent by
    /// [`crate::Job::run`] / [`crate::Job::run_with`]. Defaults to enabled.
    /// Ignored by the direct pings.
    pub fn start_ping(mut self, enabled: bool) -> Self {
        self.start_ping = enabled;
        self
    }
}

/// Server-side maximum length (in characters) of the `host` query parameter.
const MAX_HOST_CHARS: usize = 253;

/// Server-side maximum length (in characters) of the `env` query parameter.
const MAX_ENV_CHARS: usize = 32;

/// Truncate `value` to at most `max_chars` characters, always on a UTF-8 char
/// boundary. Returns the input unchanged (no allocation) when it already fits.
fn truncate_chars(value: String, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        // `byte_idx` is the start of the first character to drop.
        Some((byte_idx, _)) => value[..byte_idx].to_string(),
        None => value,
    }
}

/// Outcome of a single [`crate::Job::success`] / [`crate::Job::fail`] ping.
///
/// Ping methods never `Err`/panic on transport problems — the failure is
/// reported here (and to the client's `on_error` hook) so that a monitoring
/// outage can never take down the host application. (Usage errors such as an
/// oversized payload are the exception: they are returned as `Err` from the
/// ping method, never inside a `PingResult`.)
#[derive(Debug)]
#[non_exhaustive]
pub struct PingResult {
    /// `true` when the server acknowledged the ping with a 2xx response.
    pub ok: bool,

    /// The HTTP status code of the final response, if the request reached the
    /// server. `None` for transport failures.
    pub status: Option<u16>,

    /// The captured error when `ok` is `false`; `None` on success.
    pub error: Option<Error>,
}

/// Context handed to the [`ClientBuilder::on_error`] hook alongside the
/// [`Error`], identifying which ping failed.
///
/// A single `on_error` hook is typically shared across many jobs; without this
/// context it cannot tell *which* job's ping failed. It carries the job token
/// and whether the failing ping was a `fail` ping so the hook can attribute and
/// route the failure. The token is a credential, so the `Debug` impl **redacts
/// it** — matching the redaction on [`crate::Job`] and [`crate::Client`].
///
/// [`ClientBuilder::on_error`]: crate::ClientBuilder::on_error
#[derive(Clone)]
#[non_exhaustive]
pub struct PingContext {
    token: Arc<str>,
    fail: bool,
}

impl PingContext {
    /// Build a context for a ping on `token`. `fail` is `true` for a `/fail`
    /// ping, `false` for a success ping.
    pub(crate) fn new(token: Arc<str>, fail: bool) -> Self {
        Self { token, fail }
    }

    /// The ping token of the job whose ping failed.
    ///
    /// This is a credential; a shared hook may use it to attribute the failure,
    /// but should avoid logging it verbatim. (The [`fmt::Debug`] impl redacts
    /// it precisely so an accidental `{:?}` never leaks it.)
    pub fn token(&self) -> &str {
        &self.token
    }

    /// `true` when the ping that failed was a **fail** ping (the `/fail`
    /// variant, i.e. from [`crate::Job::fail`] / [`crate::Job::fail_with`] or
    /// the failure branch of [`crate::Job::run`]); `false` for a success ping.
    pub fn is_fail(&self) -> bool {
        self.fail
    }
}

impl fmt::Debug for PingContext {
    /// Redacts the token (a credential); the fail-kind flag is not sensitive and
    /// is shown to keep the hook debuggable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PingContext")
            .field("token", &"<redacted>")
            .field("fail", &self.fail)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_context_exposes_token_and_fail_kind() {
        let success_ctx = PingContext::new(Arc::from("your-job-token"), false);
        assert_eq!(success_ctx.token(), "your-job-token");
        assert!(!success_ctx.is_fail());

        let fail_ctx = PingContext::new(Arc::from("your-job-token"), true);
        assert!(fail_ctx.is_fail());
    }

    #[test]
    fn ping_context_debug_redacts_token() {
        let ctx = PingContext::new(Arc::from("super-secret-token"), true);
        let rendered = format!("{ctx:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "token leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // The fail-kind flag is not a secret and should stay visible.
        assert!(rendered.contains("true"));
    }

    #[test]
    fn host_is_truncated_to_253_chars() {
        let opts = PingOptions::new().host("h".repeat(300));
        let host = opts.host.expect("host set");
        assert_eq!(host.chars().count(), 253);
    }

    #[test]
    fn env_is_truncated_to_32_chars() {
        let opts = PingOptions::new().env("e".repeat(50));
        let env = opts.env.expect("env set");
        assert_eq!(env.chars().count(), 32);
    }

    #[test]
    fn host_and_env_under_limit_are_unchanged() {
        let opts = PingOptions::new().host("db-1.internal").env("production");
        assert_eq!(opts.host.as_deref(), Some("db-1.internal"));
        assert_eq!(opts.env.as_deref(), Some("production"));
    }

    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        // 300 two-byte characters truncated to 253 chars must not split a char.
        let opts = PingOptions::new().host("é".repeat(300));
        let host = opts.host.expect("host set");
        assert_eq!(host.chars().count(), 253);
        assert_eq!(host.len(), 253 * 2, "each 'é' is two bytes");
    }

    #[test]
    fn start_ping_defaults_true_and_is_settable() {
        assert!(PingOptions::new().start_ping, "start ping defaults on");
        assert!(!PingOptions::new().start_ping(false).start_ping);
    }
}
