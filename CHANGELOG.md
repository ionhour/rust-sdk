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
- `Job::success` / `Job::ping` / `Job::fail` — return
  `Result<PingResult, Error>`. Transport failures and non-2xx statuses stay
  inside `Ok(PingResult { ok: false, .. })` (and reach the `on_error` hook);
  `Err` is reserved for usage errors (`PayloadTooLarge`, invalid config), which
  make no network call.
- `Job::run` — times a fallible async operation, pings success with the measured
  duration on `Ok`, pings fail with the error message on `Err`, re-returns the
  original error, and swallows all of its own ping errors.
- `PingOptions` builder (`PingOptions::new().rid(..).duration_ms(..).payload(..)`);
  the type is `#[non_exhaustive]`.
- Credential-safe `Debug` for `Client` and `Job` (token and API key redacted).
- Client-side rejection of payloads larger than 10 KiB
  (`Error::PayloadTooLarge`).
- Retry policy: 3 attempts with 1s/2s backoff, retrying only network errors,
  HTTP 429, and 5xx.
- `test-util` crate feature gating test-only builder hooks
  (`ClientBuilder::backoff`).

[0.1.0]: https://github.com/ionhour/ionhour-rs/releases/tag/v0.1.0
