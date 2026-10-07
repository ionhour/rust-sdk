# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-10

### Added

- Initial release of the `ionhour` crate.
- `Client` with `Client::new()` (defaults + `IONHOUR_API_KEY` /
  `IONHOUR_BASE_URL` env fallbacks) and `Client::builder()` for explicit
  configuration (`api_key`, `base_url`, `timeout`, `retries`, `user_agent`,
  `on_error`). `Client` is cheaply cloneable.
- `Client::register_job(token)` and
  `Client::register_job_with_key(token, api_key)` — local, non-network job
  registration with token validation (`Error::InvalidToken`). API keys are
  trimmed before use so whitespace from a `.env` never leaks into the Bearer
  header.
- Zero-ceremony pings: `Job::success` / `Job::ping` / `Job::fail` and
  `Job::run(fn)` take **no arguments**; the telemetry-carrying forms are
  `Job::success_with` / `Job::ping_with` / `Job::fail_with` / `Job::run_with`,
  which accept `PingOptions`. The bare forms delegate to the `_with` forms with
  `PingOptions::default()`. All return `Result<PingResult, Error>` (except
  `run`, which returns the closure's own result); transport failures and non-2xx
  statuses stay inside `Ok(PingResult { ok: false, .. })` (and reach the
  `on_error` hook), while `Err` is reserved for usage errors (`PayloadTooLarge`,
  invalid config), which make no network call.
- `Job::start` / `Job::start_with` — send a **start** ping to
  `/signals/ping/{token}/start` (GET, or POST when a payload is present). Start
  pings are persist-only run telemetry: they never change monitor status, open
  or resolve incidents, or reset the schedule. Same `Result<PingResult, Error>`
  contract and URL scrubbing as the other pings.
- `Job::run` / `Job::run_with` — time a fallible async operation, send a
  best-effort start ping before it, ping success with the measured duration on
  `Ok`, ping fail with the error message on `Err`, re-return the original error,
  and swallow all of their own ping errors (start ping included). When the caller
  supplies no `rid`, one is generated (dependency-free: wall-clock nanoseconds +
  a process-wide counter) and shared by the start and terminal pings so they
  correlate. The start ping carries `rid`/`host`/`env` only, and can be disabled
  with `PingOptions::start_ping(false)`.
- `PingOptions` builder now also carries telemetry:
  `host` (silently truncated to 253 chars on a UTF-8 boundary), `env`
  (truncated to 32 chars), and `exit_code` (a `u8`, so 0–255 is a compile-time
  guarantee), mapped to the `host=`/`env=`/`exit_code=` query params; plus
  `start_ping` (default `true`, run-only). Existing setters: `rid`,
  `duration_ms`, `payload`. The type is `#[non_exhaustive]` and implements
  `Default`.
- `on_error` hook receives a `PingContext` alongside the error
  (`Fn(&Error, &PingContext)`), exposing the job token (`PingContext::token`)
  and whether the failing ping was a fail ping (`PingContext::is_fail`) so a
  shared hook can attribute failures. Its `Debug` redacts the token.
- Credential-safe `Debug` for `Client`, `Job`, and `PingContext` (token and API
  key redacted).
- Transport errors are scrubbed of the request URL (`reqwest::Error::without_url`)
  before being wrapped in `Error::Transport`, so the job token embedded in the
  ping URL never leaks through `PingResult.error`, the `on_error` hook text, or
  any caller log. The hook still receives the token via `PingContext`.
- Client-side rejection of payloads larger than 10 KiB
  (`Error::PayloadTooLarge`).
- Retry policy: 3 attempts (by default) retrying only network errors, HTTP 429,
  and 5xx. Backoff is clamped — 1s before the first retry, then 2s before every
  subsequent retry (1s, 2s, 2s, …), never exponential.
- `test-util` crate feature gating test-only builder hooks
  (`ClientBuilder::backoff`).

[0.1.0]: https://github.com/ionhour/rust-sdk/releases/tag/v0.1.0
