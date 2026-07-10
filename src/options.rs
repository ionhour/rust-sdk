//! Per-ping options and the result type returned by ping methods.

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
///     .payload(json!({ "records": 10 }));
/// ```
#[derive(Debug, Clone, Default)]
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
}

impl PingOptions {
    /// Create an empty set of options (all fields unset). Equivalent to
    /// [`PingOptions::default`].
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
