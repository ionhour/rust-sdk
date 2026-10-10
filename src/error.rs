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
/// - Usage / programmer errors ([`Error::InvalidToken`],
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
    #[error("ionhour responded with HTTP {status}")]
    Http {
        /// The HTTP status code returned by the server.
        status: u16,
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
