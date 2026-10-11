//! The [`Client`] and its [`ClientBuilder`].

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;

use crate::error::Error;
use crate::job::{Job, JobTarget};
use crate::options::{PingContext, PingOptions, PingResult, SlugJobOptions};
use crate::slug::{normalize_api_key, validate_api_key, validate_slug, SlugReply};

/// Default API base URL. Override with `IONHOUR_BASE_URL` or the builder.
pub const DEFAULT_BASE_URL: &str = "https://signal.ionhour.com/api";

/// Default per-attempt request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default total number of attempts (1 initial + 2 retries).
pub const DEFAULT_RETRIES: u32 = 3;

/// Maximum serialized payload size accepted by the server, in bytes (10 KiB).
pub const MAX_PAYLOAD_BYTES: usize = 10_240;

/// Default API host for slug-mode pings (`/signals/s/{slug}`). The signal host
/// serves only token pings. Override with `IONHOUR_API_BASE_URL` or
/// [`ClientBuilder::api_base_url`].
pub const DEFAULT_API_BASE_URL: &str = "https://api.ionhour.com/api";

/// Environment variable read for the default base URL.
const ENV_BASE_URL: &str = "IONHOUR_BASE_URL";

/// Environment variable read for the slug-mode API base URL.
const ENV_API_BASE_URL: &str = "IONHOUR_API_BASE_URL";

/// Environment variable read for the slug-mode API key.
const ENV_API_KEY: &str = "IONHOUR_API_KEY";

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
    /// Slug-mode API host. An env value is parsed lazily: a malformed
    /// `IONHOUR_API_BASE_URL` is kept as its error message and reported only by
    /// slug registration, so token-only clients are never affected.
    api_base: Result<Url, String>,
    /// Slug-mode key, trimmed; `None` when absent/blank. Never printed and
    /// never sent on token pings.
    api_key: Option<Arc<str>>,
    retries: u32,
    backoff: Vec<Duration>,
    on_error: Option<OnError>,
}

impl Client {
    /// Create a client with default configuration and environment fallbacks.
    ///
    /// Reads `IONHOUR_BASE_URL` when present. This
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

    /// Register a job by its ping token.
    ///
    /// Performs **no** network call. The token is validated (non-empty, no
    /// whitespace) and [`Error::InvalidToken`] is returned otherwise. Pings
    /// authenticate by the token alone and send no `Authorization` header.
    pub fn register_job(&self, job_token: impl Into<String>) -> Result<Job, Error> {
        let token = validate_token(job_token.into())?;
        Ok(Job::new(self.clone(), token))
    }

    /// Register a job by its slug (`^[a-z0-9_-]{1,64}$`) in the workspace of
    /// the client's API key — slug mode.
    ///
    /// Pings go to `{api_base_url}/signals/s/{slug}` (plus `/fail`, `/start`)
    /// with `Authorization: Bearer <api_key>`. Performs **no** network call.
    /// Returns [`Error::InvalidSlug`], [`Error::MissingApiKey`] or
    /// [`Error::InvalidApiKey`] on misuse. Use
    /// [`Client::register_job_by_slug_with`] to enable auto-creation.
    ///
    /// ```no_run
    /// # async fn demo() -> Result<(), ionhour::Error> {
    /// // Reads IONHOUR_API_KEY (and optionally IONHOUR_API_BASE_URL).
    /// let job = ionhour::Client::new().register_job_by_slug("nightly-report")?;
    /// job.success().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn register_job_by_slug(&self, slug: impl Into<String>) -> Result<Job, Error> {
        self.register_job_by_slug_with(slug, SlugJobOptions::default())
    }

    /// [`Client::register_job_by_slug`] with [`SlugJobOptions`]: `create(true)`
    /// sends `?create=1` so the first ping auto-creates the job (server flag
    /// `job_auto_provisioning`; [`PingResult::created`] reports it).
    pub fn register_job_by_slug_with(
        &self,
        slug: impl Into<String>,
        opts: SlugJobOptions,
    ) -> Result<Job, Error> {
        let slug = validate_slug(slug.into())?;
        self.inner.api_base_url()?;
        validate_api_key(self.inner.api_key.as_deref())?;
        Ok(Job::new_slug(self.clone(), slug, opts.create))
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
            .field("api_base", &self.inner.api_base.as_ref().map(Url::as_str))
            .field("retries", &self.inner.retries)
            .field("backoff", &self.inner.backoff)
            .field("on_error", &self.inner.on_error.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl ClientInner {
    /// The slug-mode API host, or the deferred configuration error.
    fn api_base_url(&self) -> Result<&Url, Error> {
        self.api_base
            .as_ref()
            .map_err(|message| Error::InvalidConfig(message.clone()))
    }

    /// Send a single ping, applying the retry policy. Never panics.
    ///
    /// Usage errors — an oversized or unserializable payload, or a base URL
    /// that cannot form a request — are returned as `Err` and are LOUD: no
    /// network call is made and the `on_error` hook is not invoked. Transport
    /// failures and non-2xx HTTP statuses are SILENT: they are reported inside
    /// `Ok(PingResult { ok: false, .. })` and to the `on_error` hook.
    pub(crate) async fn send_ping(
        &self,
        target: &JobTarget,
        kind: PingKind,
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

        let url = self.build_url(target, kind, opts)?;
        let slug_mode = matches!(target, JobTarget::Slug { .. });
        // Validated at registration; only slug pings carry a credential.
        let bearer = if slug_mode {
            self.api_key.as_deref()
        } else {
            None
        };

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.attempt_once(&url, body.as_deref(), bearer).await {
                Ok((status, reply)) if (200..300).contains(&status) => {
                    return Ok(PingResult {
                        ok: true,
                        status: Some(status),
                        error: None,
                        created: slug_mode && (status == 201 || reply.created),
                    });
                }
                Ok((status, reply)) => {
                    if is_retryable_status(status) && attempt < self.retries {
                        self.sleep_backoff(attempt).await;
                        continue;
                    }
                    return Ok(self.fail_result(
                        target,
                        kind.is_fail(),
                        Some(status),
                        Error::http(status, reply.code()),
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
                        target,
                        kind.is_fail(),
                        None,
                        Error::Transport(e.without_url()),
                    ));
                }
            }
        }
    }

    /// Perform exactly one HTTP attempt and return the status code plus the
    /// parsed slug-route body (when `bearer` marks a slug ping), or the
    /// transport error.
    async fn attempt_once(
        &self,
        url: &Url,
        body: Option<&[u8]>,
        bearer: Option<&str>,
    ) -> Result<(u16, SlugReply), reqwest::Error> {
        let request = match body {
            Some(bytes) => self
                .http
                .post(url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes.to_vec()),
            None => self.http.get(url.clone()),
        };

        let request = match bearer {
            Some(key) => request.bearer_auth(key),
            None => request,
        };

        let response = request.send().await?;
        let status = response.status().as_u16();
        if bearer.is_none() {
            return Ok((status, SlugReply::default()));
        }
        // Best-effort, capped at 4 KiB: an unreadable body is simply ignored.
        Ok((status, SlugReply::read(response).await))
    }

    /// Build the request URL: `{base}/signals/ping/{token}[/fail|/start]` for
    /// token jobs or `{api_base}/signals/s/{slug}[/fail|/start]` (+`create=1`)
    /// for slug jobs, plus the optional `rid`, `duration`, `host`, `env`, and
    /// `exit_code` query parameters.
    fn build_url(
        &self,
        target: &JobTarget,
        kind: PingKind,
        opts: &PingOptions,
    ) -> Result<Url, Error> {
        let (mut url, prefix, id, create) = match target {
            JobTarget::Token(token) => (self.base.clone(), "ping", &**token, false),
            JobTarget::Slug { slug, create } => {
                (self.api_base_url()?.clone(), "s", &**slug, *create)
            }
        };
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::InvalidConfig("base URL cannot be a base".to_string()))?;
            // Tolerate a trailing slash on the base URL (would otherwise create
            // an empty segment / double slash).
            segments.pop_if_empty();
            segments.push("signals").push(prefix).push(id);
            if let Some(suffix) = kind.path_suffix() {
                segments.push(suffix);
            }
        }

        if create {
            url.query_pairs_mut().append_pair("create", "1");
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
        target: &JobTarget,
        fail: bool,
        status: Option<u16>,
        error: Error,
    ) -> PingResult {
        self.report(target, fail, &error);
        PingResult {
            ok: false,
            status,
            error: Some(error),
            created: false,
        }
    }

    /// Invoke the `on_error` hook if one is configured, passing the failing
    /// ping's context. The [`PingContext`] is built here (a cheap `Arc` bump)
    /// only when there is a hook to receive it, so the success path pays nothing.
    fn report(&self, target: &JobTarget, fail: bool, error: &Error) {
        if let Some(hook) = &self.on_error {
            let ctx = match target {
                JobTarget::Token(token) => PingContext::new(token.clone(), fail),
                JobTarget::Slug { slug, .. } => PingContext::for_slug(slug.clone(), fail),
            };
            hook(error, &ctx);
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
///     .timeout(Duration::from_secs(5))
///     .retries(4)
///     .build()?;
/// # let _ = client;
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct ClientBuilder {
    base_url: Option<String>,
    api_base_url: Option<String>,
    api_key: Option<String>,
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

    /// Set the API base URL (default [`DEFAULT_BASE_URL`]).
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Set the API host used by slug-mode jobs (default
    /// [`DEFAULT_API_BASE_URL`], env `IONHOUR_API_BASE_URL`). Token pings keep
    /// using [`ClientBuilder::base_url`].
    pub fn api_base_url(mut self, api_base_url: impl Into<String>) -> Self {
        self.api_base_url = Some(api_base_url.into());
        self
    }

    /// Set the `ionh_` API key used ONLY by slug-mode jobs, sent as
    /// `Authorization: Bearer <key>`. Token pings never send it. Trimmed; a
    /// blank value counts as absent and falls back to `IONHOUR_API_KEY`.
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
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
        let api_base = resolve_api_base(self.api_base_url)?;
        let api_key = normalize_api_key(self.api_key)
            .or_else(|| normalize_api_key(std::env::var(ENV_API_KEY).ok()))
            .map(Arc::<str>::from);

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
                api_base,
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
            .field("base_url", &self.base_url)
            .field("api_base_url", &self.api_base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
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

/// Resolve the slug-mode API host. An explicit builder value is validated now
/// (loud, like `base_url`); an env value is validated lazily so a malformed
/// `IONHOUR_API_BASE_URL` never breaks a token-only client.
fn resolve_api_base(explicit: Option<String>) -> Result<Result<Url, String>, Error> {
    if let Some(raw) = explicit {
        return parse_base_url(&raw).map(Ok);
    }
    let raw = std::env::var(ENV_API_BASE_URL)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string());
    Ok(parse_base_url(&raw).map_err(|error| match error {
        Error::InvalidConfig(message) => message,
        other => other.to_string(),
    }))
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
