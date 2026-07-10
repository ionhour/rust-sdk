//! The [`Client`] and its [`ClientBuilder`].

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;

use crate::error::Error;
use crate::job::Job;
use crate::options::{PingContext, PingOptions, PingResult};

/// Default API base URL. Override with `IONHOUR_BASE_URL` or the builder.
pub const DEFAULT_BASE_URL: &str = "https://signal.ionhour.com/api";

/// Default per-attempt request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default total number of attempts (1 initial + 2 retries).
pub const DEFAULT_RETRIES: u32 = 3;

/// Maximum serialized payload size accepted by the server, in bytes (10 KiB).
pub const MAX_PAYLOAD_BYTES: usize = 10_240;

/// Environment variable read for the default API key.
const ENV_API_KEY: &str = "IONHOUR_API_KEY";

/// Environment variable read for the default base URL.
const ENV_BASE_URL: &str = "IONHOUR_BASE_URL";

/// Default `User-Agent`, e.g. `ionhour-rust/0.1.0`.
const DEFAULT_USER_AGENT: &str = concat!("ionhour-rust/", env!("CARGO_PKG_VERSION"));

/// A callback invoked whenever a ping fails, receiving the [`Error`] plus a
/// [`PingContext`] identifying the ping. See [`ClientBuilder::on_error`].
type OnError = Arc<dyn Fn(&Error, &PingContext) + Send + Sync>;

/// Which ping endpoint a request targets. Internal; the public surface exposes
/// these as the `success`/`fail`/`start` method families on [`Job`].
#[derive(Clone, Copy)]
pub(crate) enum PingKind {
    Success,
    Fail,
    Start,
}

impl PingKind {
    /// The path segment appended after the token, if any (`success` has none).
    fn path_suffix(self) -> Option<&'static str> {
        match self {
            PingKind::Success => None,
            PingKind::Fail => Some("fail"),
            PingKind::Start => Some("start"),
        }
    }

    /// Whether this is a `fail` ping — carried in the `on_error` [`PingContext`].
    /// A `start` ping is telemetry, not a failure, so it reports `false`.
    fn is_fail(self) -> bool {
        matches!(self, PingKind::Fail)
    }
}

/// A cheaply-cloneable Ionhour client.
///
/// Cloning is cheap (an `Arc` bump); share one `Client` across your whole
/// application and derive [`Job`]s from it with [`Client::register_job`].
///
/// ```no_run
/// use ionhour::Client;
///
/// # async fn demo() -> Result<(), ionhour::Error> {
/// let client = Client::new();
/// let job = client.register_job("your-job-token")?;
/// job.success().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

/// Shared, immutable client state behind the `Arc`.
pub(crate) struct ClientInner {
    http: reqwest::Client,
    base: Url,
    api_key: Option<String>,
    retries: u32,
    backoff: Vec<Duration>,
    on_error: Option<OnError>,
}

impl Client {
    /// Create a client with default configuration and environment fallbacks.
    ///
    /// Reads `IONHOUR_API_KEY` and `IONHOUR_BASE_URL` when present. This
    /// constructor is infallible: if the environment supplies a malformed
    /// `IONHOUR_BASE_URL`, it falls back to [`DEFAULT_BASE_URL`] rather than
    /// failing, so that constructing a monitor never takes down the host
    /// application. Use [`Client::builder`] when you want configuration errors
    /// surfaced as a [`Result`].
    pub fn new() -> Client {
        match Self::builder().build() {
            Ok(client) => client,
            Err(_) => {
                // A bad ambient `IONHOUR_BASE_URL` is the only realistic way the
                // default build fails. Retry pinning the built-in base URL,
                // which is always valid.
                Self::builder()
                    .base_url(DEFAULT_BASE_URL)
                    .build()
                    .expect("default configuration is always valid")
            }
        }
    }

    /// Start building a client with explicit configuration.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Register a job by its ping token, inheriting the client's default API
    /// key (if any).
    ///
    /// Performs **no** network call. The token is validated (non-empty, no
    /// whitespace) and [`Error::InvalidToken`] is returned otherwise.
    pub fn register_job(&self, job_token: impl Into<String>) -> Result<Job, Error> {
        let token = validate_token(job_token.into())?;
        Ok(Job::new(self.clone(), token, self.inner.api_key.clone()))
    }

    /// Register a job with an explicit per-job API key that **overrides** the
    /// client's default key for this job's pings.
    ///
    /// Performs **no** network call. The token is validated (non-empty, no
    /// whitespace) and [`Error::InvalidToken`] is returned otherwise. An empty
    /// or whitespace-only API key is treated as "no key" and no `Authorization`
    /// header is sent.
    pub fn register_job_with_key(
        &self,
        job_token: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Job, Error> {
        let token = validate_token(job_token.into())?;
        let key = api_key.into();
        // Trim before storing: a `.env`-sourced key like " ionh_x\n" would
        // otherwise produce a malformed "Bearer  ionh_x\n" header.
        let trimmed = key.trim();
        let key = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        Ok(Job::new(self.clone(), token, key))
    }

    pub(crate) fn inner(&self) -> &ClientInner {
        &self.inner
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.inner.base.as_str())
            .field(
                "api_key",
                &self.inner.api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("retries", &self.inner.retries)
            .field("backoff", &self.inner.backoff)
            .field("on_error", &self.inner.on_error.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl ClientInner {
    /// Send a single ping, applying the retry policy. Never panics.
    ///
    /// Usage errors — an oversized or unserializable payload, or a base URL
    /// that cannot form a request — are returned as `Err` and are LOUD: no
    /// network call is made and the `on_error` hook is not invoked. Transport
    /// failures and non-2xx HTTP statuses are SILENT: they are reported inside
    /// `Ok(PingResult { ok: false, .. })` and to the `on_error` hook.
    pub(crate) async fn send_ping(
        &self,
        token: &Arc<str>,
        kind: PingKind,
        api_key: Option<&str>,
        opts: &PingOptions,
    ) -> Result<PingResult, Error> {
        // Serialize + size-guard the payload up front. A usage error here is
        // returned as `Err` and must not reach the network.
        let body = match &opts.payload {
            Some(value) => match serde_json::to_vec(value) {
                Ok(bytes) if bytes.len() > MAX_PAYLOAD_BYTES => {
                    return Err(Error::PayloadTooLarge { size: bytes.len() });
                }
                Ok(bytes) => Some(bytes),
                Err(e) => {
                    return Err(Error::InvalidConfig(format!(
                        "payload is not serializable: {e}"
                    )));
                }
            },
            None => None,
        };

        let url = self.build_url(token, kind, opts)?;

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.attempt_once(&url, body.as_deref(), api_key).await {
                Ok(status) if (200..300).contains(&status) => {
                    return Ok(PingResult {
                        ok: true,
                        status: Some(status),
                        error: None,
                    });
                }
                Ok(status) => {
                    if is_retryable_status(status) && attempt < self.retries {
                        self.sleep_backoff(attempt).await;
                        continue;
                    }
                    return Ok(self.fail_result(
                        token,
                        kind.is_fail(),
                        Some(status),
                        Error::Http { status },
                    ));
                }
                Err(e) => {
                    let retryable = e.is_timeout() || e.is_connect();
                    if retryable && attempt < self.retries {
                        self.sleep_backoff(attempt).await;
                        continue;
                    }
                    // Scrub the request URL before wrapping: reqwest's Display
                    // embeds the full URL, whose path carries the job token, so
                    // an un-scrubbed transport error would leak the credential
                    // through PingResult.error, the on_error hook text, and any
                    // caller log. The hook still receives the token via
                    // PingContext, so nothing is lost.
                    return Ok(self.fail_result(
                        token,
                        kind.is_fail(),
                        None,
                        Error::Transport(e.without_url()),
                    ));
                }
            }
        }
    }

    /// Perform exactly one HTTP attempt and return the status code, or the
    /// transport error.
    async fn attempt_once(
        &self,
        url: &Url,
        body: Option<&[u8]>,
        api_key: Option<&str>,
    ) -> Result<u16, reqwest::Error> {
        let mut request = match body {
            Some(bytes) => self
                .http
                .post(url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes.to_vec()),
            None => self.http.get(url.clone()),
        };

        if let Some(key) = api_key {
            if !key.is_empty() {
                request = request.bearer_auth(key);
            }
        }

        let response = request.send().await?;
        Ok(response.status().as_u16())
    }

    /// Build the request URL: `{base}/signals/ping/{token}[/fail|/start]` plus
    /// the optional `rid`, `duration`, `host`, `env`, and `exit_code` query
    /// parameters.
    fn build_url(&self, token: &str, kind: PingKind, opts: &PingOptions) -> Result<Url, Error> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::InvalidConfig("base URL cannot be a base".to_string()))?;
            // Tolerate a trailing slash on the base URL (would otherwise create
            // an empty segment / double slash).
            segments.pop_if_empty();
            segments.push("signals").push("ping").push(token);
            if let Some(suffix) = kind.path_suffix() {
                segments.push(suffix);
            }
        }

        if opts.rid.is_some()
            || opts.duration_ms.is_some()
            || opts.host.is_some()
            || opts.env.is_some()
            || opts.exit_code.is_some()
        {
            let mut query = url.query_pairs_mut();
            if let Some(rid) = &opts.rid {
                query.append_pair("rid", rid);
            }
            if let Some(duration) = opts.duration_ms {
                query.append_pair("duration", &duration.to_string());
            }
            if let Some(host) = &opts.host {
                query.append_pair("host", host);
            }
            if let Some(env) = &opts.env {
                query.append_pair("env", env);
            }
            if let Some(exit_code) = opts.exit_code {
                query.append_pair("exit_code", &exit_code.to_string());
            }
        }

        Ok(url)
    }

    /// Sleep the configured backoff before the next attempt. `attempt` is the
    /// 1-based number of the attempt that just failed; the wait before attempt
    /// N+1 is `backoff[N-1]` (missing entries mean no wait).
    async fn sleep_backoff(&self, attempt: u32) {
        let idx = (attempt - 1) as usize;
        let wait = self.backoff.get(idx).copied().unwrap_or(Duration::ZERO);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// Build a failed [`PingResult`] for a silent (transport / HTTP) failure and
    /// notify the `on_error` hook.
    fn fail_result(
        &self,
        token: &Arc<str>,
        fail: bool,
        status: Option<u16>,
        error: Error,
    ) -> PingResult {
        self.report(token, fail, &error);
        PingResult {
            ok: false,
            status,
            error: Some(error),
        }
    }

    /// Invoke the `on_error` hook if one is configured, passing the failing
    /// ping's context. The [`PingContext`] is built here (a cheap `Arc` bump)
    /// only when there is a hook to receive it, so the success path pays nothing.
    fn report(&self, token: &Arc<str>, fail: bool, error: &Error) {
        if let Some(hook) = &self.on_error {
            hook(error, &PingContext::new(token.clone(), fail));
        }
    }
}

/// Builder for [`Client`]. Configuration precedence is:
/// explicit builder value → environment variable → default.
///
/// ```no_run
/// use std::time::Duration;
/// use ionhour::Client;
///
/// # fn demo() -> Result<(), ionhour::Error> {
/// let client = Client::builder()
///     .api_key("ionh_live_xxx")
///     .timeout(Duration::from_secs(5))
///     .retries(4)
///     .build()?;
/// # let _ = client;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    timeout: Option<Duration>,
    retries: Option<u32>,
    user_agent: Option<String>,
    on_error: Option<OnError>,
    backoff: Option<Vec<Duration>>,
}

impl ClientBuilder {
    /// Create a builder with all values unset (defaults/env applied at
    /// [`ClientBuilder::build`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the workspace API key sent as `Authorization: Bearer <key>`. This is
    /// the client-wide default; a per-job key from
    /// [`Client::register_job_with_key`] overrides it.
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the API base URL (default [`DEFAULT_BASE_URL`]).
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Set the per-attempt request timeout (default [`DEFAULT_TIMEOUT`]).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the total number of attempts, including the first (default
    /// [`DEFAULT_RETRIES`]). A value of `1` disables retries.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }

    /// Set the `User-Agent` header (default `ionhour-rust/<version>`).
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Register a callback invoked for each *silent* ping failure — a transport
    /// error or a non-2xx HTTP status carried inside a returned [`PingResult`].
    ///
    /// The callback receives the [`Error`] plus a [`PingContext`] identifying
    /// the ping — the job token and whether it was a `fail` ping — so a single
    /// hook shared across many jobs can attribute and route each failure.
    ///
    /// Usage errors (oversized payload, invalid config) are returned as `Err`
    /// from the ping methods instead and do **not** reach this hook. Use it to
    /// log or count delivery failures. The callback must be cheap and must not
    /// panic.
    ///
    /// ```
    /// use ionhour::Client;
    /// let client = Client::builder()
    ///     .on_error(|err, ctx| {
    ///         eprintln!(
    ///             "ionhour ping failed for {} (fail_ping={}): {err}",
    ///             ctx.token(),
    ///             ctx.is_fail(),
    ///         )
    ///     })
    ///     .build()
    ///     .unwrap();
    /// # let _ = client;
    /// ```
    pub fn on_error(
        mut self,
        on_error: impl Fn(&Error, &PingContext) + Send + Sync + 'static,
    ) -> Self {
        self.on_error = Some(Arc::new(on_error));
        self
    }

    /// Override the retry backoff schedule (one [`Duration`] per wait between
    /// attempts).
    ///
    /// Available only with the `test-util` crate feature; it exists so tests can
    /// zero out delays for fast retry assertions. Production code should use
    /// [`ClientBuilder::retries`], which derives the clamped 1s-then-2s
    /// schedule (see [`default_backoff`]).
    #[cfg(feature = "test-util")]
    pub fn backoff(mut self, backoff: Vec<Duration>) -> Self {
        self.backoff = Some(backoff);
        self
    }

    /// Finalize configuration and build the [`Client`], applying environment
    /// fallbacks and validating the base URL.
    pub fn build(self) -> Result<Client, Error> {
        let base_raw = self
            .base_url
            .or_else(|| std::env::var(ENV_BASE_URL).ok())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base = parse_base_url(&base_raw)?;

        // Trim the client-level key (explicit or from `IONHOUR_API_KEY`) before
        // storing so whitespace can never leak into the Bearer header.
        let api_key = self
            .api_key
            .or_else(|| std::env::var(ENV_API_KEY).ok())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());

        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let retries = self.retries.unwrap_or(DEFAULT_RETRIES).max(1);
        let user_agent = self
            .user_agent
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
        let backoff = self.backoff.unwrap_or_else(|| default_backoff(retries));

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(user_agent)
            .build()?;

        Ok(Client {
            inner: Arc::new(ClientInner {
                http,
                base,
                api_key,
                retries,
                backoff,
                on_error: self.on_error,
            }),
        })
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("retries", &self.retries)
            .field("user_agent", &self.user_agent)
            .field("on_error", &self.on_error.as_ref().map(|_| "<fn>"))
            .field("backoff", &self.backoff)
            .finish()
    }
}

/// Derive the default backoff schedule covering the waits between `retries`
/// attempts: 1s for the first wait, then **2s clamped** for every subsequent
/// wait (1s, 2s, 2s, 2s, …). The clamp keeps a burst of retries from stretching
/// into minute-long tail waits — a heartbeat that cannot reach the server should
/// keep trying at a steady cadence, not back off unboundedly.
fn default_backoff(retries: u32) -> Vec<Duration> {
    (0..retries.saturating_sub(1))
        .map(|i| {
            if i == 0 {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(2)
            }
        })
        .collect()
}

/// Validate and normalize the base URL.
fn parse_base_url(raw: &str) -> Result<Url, Error> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = Url::parse(trimmed)
        .map_err(|e| Error::InvalidConfig(format!("invalid base URL {raw:?}: {e}")))?;
    if url.cannot_be_a_base() || url.host().is_none() {
        return Err(Error::InvalidConfig(format!(
            "invalid base URL {raw:?}: must be an absolute http(s) URL"
        )));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::InvalidConfig(format!(
            "invalid base URL {raw:?}: scheme must be http or https"
        )));
    }
    Ok(url)
}

/// A token is valid when it is non-empty and contains no whitespace.
fn validate_token(token: String) -> Result<String, Error> {
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(Error::InvalidToken);
    }
    Ok(token)
}

/// 429 and any 5xx are retryable; all other statuses are not.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backoff_clamps_at_two_seconds() {
        // The classic 3-attempt schedule: one 1s wait, then a 2s wait.
        assert_eq!(
            default_backoff(3),
            vec![Duration::from_secs(1), Duration::from_secs(2)]
        );

        // With more retries the schedule is 1s, then 2s for EVERY subsequent
        // wait — never exponential (which would give 4s, 8s, …).
        let schedule = default_backoff(6);
        assert_eq!(
            schedule,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_secs(2),
            ]
        );
        assert_eq!(
            schedule[3],
            Duration::from_secs(2),
            "the 4th wait must be clamped to 2s, not grown exponentially"
        );

        assert_eq!(default_backoff(1), Vec::<Duration>::new());
        assert_eq!(default_backoff(0), Vec::<Duration>::new());
    }

    #[test]
    fn retryable_status_classification() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(200));
    }

    #[test]
    fn token_validation() {
        assert!(validate_token("abc".to_string()).is_ok());
        assert!(matches!(
            validate_token(String::new()),
            Err(Error::InvalidToken)
        ));
        assert!(matches!(
            validate_token("has space".to_string()),
            Err(Error::InvalidToken)
        ));
        assert!(matches!(
            validate_token("tab\there".to_string()),
            Err(Error::InvalidToken)
        ));
    }

    #[test]
    fn base_url_validation() {
        assert!(parse_base_url("https://signal.ionhour.com/api").is_ok());
        assert!(parse_base_url("http://localhost:3000/api").is_ok());
        assert!(matches!(
            parse_base_url("not a url"),
            Err(Error::InvalidConfig(_))
        ));
        assert!(matches!(
            parse_base_url("ftp://example.com"),
            Err(Error::InvalidConfig(_))
        ));
    }
}
