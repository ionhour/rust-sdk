# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-10

### Added

- Initial release of the `ionhour` crate.
- `Client` with `Client::new()` (defaults + `IONHOUR_BASE_URL` env fallback)
  and `Client::builder()` for explicit configuration (`base_url`, `timeout`,
  `retries`, `user_agent`, `on_error`). `Client` is cheaply cloneable.
- `Client::register_job(token)` — local, non-network job registration with
  token validation (`Error::InvalidToken`).
- Token pings authenticate by job token alone and never send an
  `Authorization` header, even with `api_key` or `IONHOUR_API_KEY` set (both
  are slug-mode only). Protected jobs, which require an API key on pings, will
  ship with their own option.
- Slug mode: `Client::register_job_by_slug(slug)` and
  `register_job_by_slug_with(slug, SlugJobOptions::new().create(true))`. Pings
  `{api_base_url}/signals/s/{slug}` (`/fail`, `/start`) on the API host with
  `Authorization: Bearer <key>`, adding `?create=1` when `create` is set. New
  builder methods `api_base_url` (`IONHOUR_API_BASE_URL`, default
  `DEFAULT_API_BASE_URL` = `https://api.ionhour.com/api`) and `api_key`
  (`IONHOUR_API_KEY`, trimmed, blank = absent). A malformed
  `IONHOUR_API_BASE_URL` is reported lazily as `Error::InvalidConfig` from slug
  registration, so token-only clients are unaffected. Registration returns
  `Error::InvalidSlug { slug_prefix }` (at most an 8-character prefix, never
  the full value), `Error::MissingApiKey` or `Error::InvalidApiKey`.
  `PingResult::created` reports auto-creation (201), `Error::Http` (now
  `#[non_exhaustive]`; match with `{ status, .. }`) carries the slug error
  `code`, at most 4 KiB of a slug body is read, and `PingContext::slug()` identifies slug jobs in the
  `on_error` hook. Slug 4xx are never retried, except 429. The key is
  redacted in every `Debug` impl.
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
- Credential-safe `Debug` for `Job` and `PingContext` (token redacted).
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
