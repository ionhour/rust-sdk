//! Error type for the Ionhour SDK.

use thiserror::Error;

/// Errors produced by the SDK.
///
/// The error philosophy is deliberate: monitoring must never take down the
/// host application.
///
/// - Transport-level problems ([`Error::Transport`], [`Error::Http`]) are
///   surfaced *inside* a [`crate::PingResult`] rather than returned from
///   [`crate::Job::success`] / [`crate::Job::fail`], which never `Err`.
/// - Usage / programmer errors ([`Error::InvalidToken`], [`Error::InvalidSlug`],
///   [`Error::MissingApiKey`], [`Error::InvalidApiKey`],
///   [`Error::PayloadTooLarge`], [`Error::InvalidConfig`]) are loud: they are
///   returned from the call that made the mistake (registration, `build`) or
///   carried in the [`crate::PingResult`] for the oversized-payload case, which
///   never reaches the network.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The job token was empty or contained whitespace.
    ///
    /// Returned by [`crate::Client::register_job`]. Registration performs no
    /// network call, so this fails fast at call time.
    #[error("invalid job token: must be non-empty and contain no whitespace")]
    InvalidToken,

    /// The slug passed to [`crate::Client::register_job_by_slug`] does not
    /// match `^[a-z0-9_-]{1,64}$`. A rejected "slug" may be a credential passed
    /// by mistake (swapped arguments), so only a short prefix is kept.
    #[error("invalid slug {slug_prefix:?}: must match ^[a-z0-9_-]{{1,64}}$")]
    InvalidSlug {
        /// At most 8 characters (and at most half) of the rejected value,
        /// followed by an ellipsis. Never the full value.
        slug_prefix: String,
    },

    /// Slug mode was used without an API key (builder `api_key` or
    /// `IONHOUR_API_KEY`). Returned at registration.
    #[error("slug mode needs an API key: set ClientBuilder::api_key or IONHOUR_API_KEY")]
    MissingApiKey,

    /// The API key contains whitespace or control characters. The key itself
    /// is never echoed.
    #[error("API key must not contain whitespace or control characters")]
    InvalidApiKey,

    /// The serialized JSON payload exceeded the server's 10 KiB limit.
    ///
    /// The ping is refused client-side before any network call; the error is
    /// returned inside the [`crate::PingResult`] (never panics/`Err`s out of a
    /// ping method).
    #[error("payload too large: {size} bytes exceeds the 10240-byte limit")]
    PayloadTooLarge {
        /// The size, in bytes, of the serialized payload that was rejected.
        size: usize,
    },

    /// The server returned a non-2xx HTTP status that the SDK will not (or can
    /// no longer) retry.
    ///
    /// `#[non_exhaustive]`: match with `Error::Http { status, .. }`.
    #[error("ionhour responded with HTTP {status}{}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    #[non_exhaustive]
    Http {
        /// The HTTP status code returned by the server.
        status: u16,
        /// The stable error code from a slug-route error body (for example
        /// `KEY_SCOPE_DENIED` or `FEATURE_DISABLED`). `None` for token pings
        /// and for bodies without a well-formed code.
        code: Option<String>,
    },

    /// A transport-level failure occurred (connection refused, timeout, DNS,
    /// TLS, etc.).
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The client was configured with an invalid value (for example a
    /// malformed base URL, or a payload that could not be serialized to JSON).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

impl Error {
    /// Build an [`Error::Http`] (non-exhaustive, so constructed in-crate only).
    pub(crate) fn http(status: u16, code: Option<String>) -> Self {
        Error::Http { status, code }
    }
}
