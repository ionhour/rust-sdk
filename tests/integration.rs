//! Integration tests for the Ionhour SDK against a mocked HTTP server
//! (`wiremock`). Covers the full required-test list from `sdks/CONTRACT.md`.
//!
//! Relies on the `test-util` crate feature (enabled via the self-referential
//! dev-dependency in Cargo.toml) for `ClientBuilder::backoff`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ionhour::{Client, Error, PingOptions};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "your-job-token";

fn ping_path() -> String {
    format!("/api/signals/ping/{TOKEN}")
}

fn fail_path() -> String {
    format!("/api/signals/ping/{TOKEN}/fail")
}

fn start_path() -> String {
    format!("/api/signals/ping/{TOKEN}/start")
}

/// Build a client pointed at the mock server with retry backoff zeroed so the
/// retry tests run instantly.
fn client_for(server: &MockServer, api_key: Option<&str>) -> Client {
    let mut builder = Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .retries(3)
        .backoff(vec![Duration::ZERO, Duration::ZERO, Duration::ZERO]);
    if let Some(key) = api_key {
        builder = builder.api_key(key);
    }
    builder.build().expect("client builds")
}

fn ok_body() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" }))
}

/// A responder that walks a fixed status sequence and counts every call, so the
/// retry tests can assert the exact number of HTTP attempts. The last status
/// repeats once the sequence is exhausted.
struct SeqResponder {
    statuses: Vec<u16>,
    calls: Arc<AtomicUsize>,
}

impl Respond for SeqResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let idx = n.min(self.statuses.len() - 1);
        ResponseTemplate::new(self.statuses[idx]).set_body_json(json!({ "message": "OK" }))
    }
}

// --- success path -----------------------------------------------------------

#[tokio::test]
async fn success_uses_get_and_correct_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // Zero-ceremony form: no options value constructed.
    let result = job.success().await.unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn success_with_payload_posts_json_body() {
    let server = MockServer::start().await;
    let payload = json!({ "records": 10, "region": "eu" });
    Mock::given(method("POST"))
        .and(path(ping_path()))
        .and(body_json(payload.clone()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .success_with(PingOptions::new().payload(payload))
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
}

#[tokio::test]
async fn ping_alias_sends_success_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // `ping()` is the no-arg alias for `success()`.
    let result = job.ping().await.unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
}

#[tokio::test]
async fn ping_with_alias_posts_payload() {
    let server = MockServer::start().await;
    let payload = json!({ "queued": 3 });
    Mock::given(method("POST"))
        .and(path(ping_path()))
        .and(body_json(payload.clone()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // `ping_with(opts)` is the alias for `success_with(opts)`.
    let result = job
        .ping_with(PingOptions::new().payload(payload))
        .await
        .unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn fail_uses_fail_suffix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(fail_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // Zero-ceremony fail ping.
    let result = job.fail().await.unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn fail_with_posts_payload_to_fail_suffix() {
    let server = MockServer::start().await;
    let payload = json!({ "reason": "disk full" });
    Mock::given(method("POST"))
        .and(path(fail_path()))
        .and(body_json(payload.clone()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .fail_with(PingOptions::new().payload(payload))
        .await
        .unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn rid_and_duration_are_sent_as_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .and(query_param("rid", "run-1"))
        .and(query_param("duration", "1500"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .success_with(PingOptions::new().rid("run-1").duration_ms(1500))
        .await
        .unwrap();

    assert!(result.ok);
}

// --- Authorization header ---------------------------------------------------

async fn auth_header_for(server: &MockServer) -> Option<String> {
    let requests = server.received_requests().await.expect("recording enabled");
    let request = requests.last().expect("one request recorded");
    request
        .headers
        .get("authorization")
        .map(|value| value.to_str().unwrap().to_string())
}

#[tokio::test]
async fn per_job_key_overrides_client_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, Some("client_key"))
        .register_job_with_key(TOKEN, "job_key")
        .unwrap();
    job.success().await.unwrap();

    assert_eq!(
        auth_header_for(&server).await.as_deref(),
        Some("Bearer job_key")
    );
}

#[tokio::test]
async fn client_key_used_when_no_per_job_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, Some("client_key"))
        .register_job(TOKEN)
        .unwrap();
    job.success().await.unwrap();

    assert_eq!(
        auth_header_for(&server).await.as_deref(),
        Some("Bearer client_key")
    );
}

#[tokio::test]
async fn no_authorization_header_when_no_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    job.success().await.unwrap();

    assert_eq!(auth_header_for(&server).await, None);
}

#[tokio::test]
async fn whitespace_padded_per_job_key_is_trimmed_in_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None)
        .register_job_with_key(TOKEN, "  ionh_padded_key\n")
        .unwrap();
    job.success().await.unwrap();

    assert_eq!(
        auth_header_for(&server).await.as_deref(),
        Some("Bearer ionh_padded_key")
    );
}

#[tokio::test]
async fn whitespace_padded_client_key_is_trimmed_in_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, Some("\tionh_client_key "))
        .register_job(TOKEN)
        .unwrap();
    job.success().await.unwrap();

    assert_eq!(
        auth_header_for(&server).await.as_deref(),
        Some("Bearer ionh_client_key")
    );
}

// --- retry policy -----------------------------------------------------------

#[tokio::test]
async fn retries_on_500_then_succeeds() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(SeqResponder {
            statuses: vec![500, 200],
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job.success().await.unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_on_429_then_succeeds() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(SeqResponder {
            statuses: vec![429, 200],
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job.success().await.unwrap();

    assert!(result.ok);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_exhausted_on_500_returns_ok_with_failure() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(SeqResponder {
            statuses: vec![500],
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // Transport/HTTP failures stay inside Ok(PingResult), never Err.
    let result = job.success().await.unwrap();

    assert!(!result.ok);
    assert_eq!(result.status, Some(500));
    assert!(matches!(result.error, Some(Error::Http { status: 500 })));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn does_not_retry_on_400() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(SeqResponder {
            statuses: vec![400],
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job.success().await.unwrap();

    assert!(!result.ok);
    assert_eq!(result.status, Some(400));
    assert!(matches!(result.error, Some(Error::Http { status: 400 })));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// --- transport failure ------------------------------------------------------

#[tokio::test]
async fn unreachable_server_returns_not_ok_without_panicking() {
    // Bind then immediately release a port so the address refuses connections.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let client = Client::builder()
        .base_url(format!("http://{addr}/api"))
        .retries(3)
        .backoff(vec![Duration::ZERO, Duration::ZERO, Duration::ZERO])
        .build()
        .unwrap();

    let job = client.register_job(TOKEN).unwrap();
    // A transport failure is a reachable outcome: Ok(PingResult { ok: false }).
    let result = job.success().await.unwrap();

    assert!(!result.ok);
    assert!(result.status.is_none());
    assert!(matches!(result.error, Some(Error::Transport(_))));
}

#[tokio::test]
async fn transport_error_text_does_not_leak_the_token() {
    // A distinctive, unmistakable token so any leak is obvious in the output.
    // It contains no characters that percent-encode, so its raw and URL-encoded
    // forms coincide — and the fix strips the whole URL anyway, removing the
    // token in every representation.
    const LEAK_TOKEN: &str = "s3cr3t-leak-token-DEADBEEF";

    // Bind then immediately release a port so connections are refused, forcing a
    // transport error whose reqwest URL would embed the token in its path.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let client = Client::builder()
        .base_url(format!("http://{addr}/api"))
        .retries(1)
        .build()
        .unwrap();
    let job = client.register_job(LEAK_TOKEN).unwrap();

    // Assert the token never appears in the error's Display or Debug for a given
    // failing ping.
    fn assert_no_leak(result_error: Option<Error>, token: &str) {
        let err = result_error.expect("a transport error should be present");
        assert!(
            matches!(err, Error::Transport(_)),
            "should be a Transport error"
        );
        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(
            !display.contains(token),
            "token leaked via Display: {display}"
        );
        assert!(!debug.contains(token), "token leaked via Debug: {debug}");
    }

    // Success-ping path.
    let success = job.success().await.unwrap();
    assert!(!success.ok);
    assert_no_leak(success.error, LEAK_TOKEN);

    // Fail-ping path (the `/fail` URL also embeds the token).
    let fail = job.fail().await.unwrap();
    assert!(!fail.ok);
    assert_no_leak(fail.error, LEAK_TOKEN);
}

// --- on_error hook context --------------------------------------------------

/// Build a client whose `on_error` hook records `(token, is_fail)` into `sink`,
/// with retries disabled so a single failing response fires the hook once.
fn client_capturing_context(
    server: &MockServer,
    sink: Arc<Mutex<Option<(String, bool)>>>,
) -> Client {
    Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .retries(1)
        .on_error(move |_err, ctx| {
            *sink.lock().unwrap() = Some((ctx.token().to_string(), ctx.is_fail()));
        })
        .build()
        .expect("client builds")
}

#[tokio::test]
async fn on_error_hook_receives_context_for_success_ping_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let captured: Arc<Mutex<Option<(String, bool)>>> = Arc::new(Mutex::new(None));
    let job = client_capturing_context(&server, captured.clone())
        .register_job(TOKEN)
        .unwrap();
    let result = job.success().await.unwrap();

    assert!(!result.ok);
    let (token, is_fail) = captured
        .lock()
        .unwrap()
        .clone()
        .expect("on_error hook fired");
    assert_eq!(token, TOKEN);
    assert!(!is_fail, "a success ping must report is_fail == false");
}

#[tokio::test]
async fn on_error_hook_receives_context_for_fail_ping_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(fail_path()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let captured: Arc<Mutex<Option<(String, bool)>>> = Arc::new(Mutex::new(None));
    let job = client_capturing_context(&server, captured.clone())
        .register_job(TOKEN)
        .unwrap();
    let result = job.fail().await.unwrap();

    assert!(!result.ok);
    let (token, is_fail) = captured
        .lock()
        .unwrap()
        .clone()
        .expect("on_error hook fired");
    assert_eq!(token, TOKEN);
    assert!(is_fail, "a fail ping must report is_fail == true");
}

#[tokio::test]
async fn on_error_hook_fires_once_after_retries_exhausted() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(SeqResponder {
            statuses: vec![500],
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    // Count how many times the hook fires across the whole 3-attempt sequence.
    let hook_hits = Arc::new(AtomicUsize::new(0));
    let hook_sink = hook_hits.clone();
    let client = Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .retries(3)
        .backoff(vec![Duration::ZERO, Duration::ZERO, Duration::ZERO])
        .on_error(move |_err, _ctx| {
            hook_sink.fetch_add(1, Ordering::SeqCst);
        })
        .build()
        .unwrap();

    let job = client.register_job(TOKEN).unwrap();
    let result = job.success().await.unwrap();

    assert!(!result.ok);
    assert_eq!(result.status, Some(500));
    // All three HTTP attempts were made against the server...
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // ...but the hook fires exactly once — on the final exhausted outcome, not
    // once per retry.
    assert_eq!(
        hook_hits.load(Ordering::SeqCst),
        1,
        "on_error must fire once per final outcome, not once per retry"
    );
}

// --- run() wrapper ----------------------------------------------------------

#[tokio::test]
async fn run_ok_sends_success_ping_with_duration() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    // Zero-ceremony run: no options value.
    let value: u64 = job
        .run(|| async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok::<u64, String>(7)
        })
        .await
        .expect("run returns the closure's Ok value");

    assert_eq!(value, 7);

    let requests = server.received_requests().await.unwrap();
    let request = requests.last().unwrap();
    assert!(!request.url.path().ends_with("/fail"));
    let has_duration = request.url.query_pairs().any(|(k, _)| k == "duration");
    assert!(has_duration, "success ping must carry a duration param");
}

#[tokio::test]
async fn run_err_sends_fail_ping_with_error_and_returns_original_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(fail_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let outcome: Result<(), String> = job
        .run(|| async { Err::<(), String>("kaboom".to_string()) })
        .await;

    assert_eq!(outcome, Err("kaboom".to_string()));

    let requests = server.received_requests().await.unwrap();
    let request = requests.last().unwrap();
    assert!(request.url.path().ends_with("/fail"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["error"], json!("kaboom"));
}

#[tokio::test]
async fn run_with_seeds_rid_on_the_success_ping() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .and(query_param("rid", "nightly"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let value: u64 = job
        .run_with(
            || async { Ok::<u64, String>(3) },
            PingOptions::new().rid("nightly"),
        )
        .await
        .expect("run_with returns the closure's Ok value");

    assert_eq!(value, 3);

    let requests = server.received_requests().await.unwrap();
    let request = requests.last().unwrap();
    // run_with keeps the caller's rid but still stamps the measured duration.
    let has_duration = request.url.query_pairs().any(|(k, _)| k == "duration");
    assert!(
        has_duration,
        "run_with success ping must carry a duration param"
    );
}

// --- start ping + telemetry -------------------------------------------------

#[tokio::test]
async fn start_uses_start_suffix_with_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(start_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job.start().await.unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn start_with_payload_posts_to_start() {
    let server = MockServer::start().await;
    let payload = json!({ "pid": 4321 });
    Mock::given(method("POST"))
        .and(path(start_path()))
        .and(body_json(payload.clone()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .start_with(PingOptions::new().payload(payload))
        .await
        .unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn telemetry_params_are_sent_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .and(query_param("host", "db-1.internal"))
        .and(query_param("env", "production"))
        .and(query_param("exit_code", "7"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .success_with(
            PingOptions::new()
                .host("db-1.internal")
                .env("production")
                .exit_code(7),
        )
        .await
        .unwrap();

    assert!(result.ok);
}

#[tokio::test]
async fn oversized_host_is_truncated_on_the_wire() {
    let server = MockServer::start().await;
    // 300-char host must arrive truncated to the server's 253-char limit.
    let truncated = "h".repeat(253);
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .and(query_param("host", truncated.as_str()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job
        .success_with(PingOptions::new().host("h".repeat(300)))
        .await
        .unwrap();

    assert!(result.ok);
}

/// The `rid` query value of a recorded request, if present.
fn rid_of(request: &Request) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(k, _)| k == "rid")
        .map(|(_, v)| v.into_owned())
}

#[tokio::test]
async fn run_sends_start_ping_and_shares_generated_rid_with_terminal() {
    let server = MockServer::start().await;
    // Terminal success ping (GET, no /start or /fail suffix).
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .mount(&server)
        .await;
    // Start ping.
    Mock::given(method("GET"))
        .and(path(start_path()))
        .respond_with(ok_body())
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let value: u64 = job.run(|| async { Ok::<u64, String>(9) }).await.unwrap();
    assert_eq!(value, 9);

    let requests = server.received_requests().await.unwrap();
    let start_req = requests
        .iter()
        .find(|r| r.url.path() == start_path())
        .expect("a start ping was sent");
    let terminal_req = requests
        .iter()
        .find(|r| r.url.path() == ping_path())
        .expect("a terminal success ping was sent");

    let start_rid = rid_of(start_req).expect("start ping carries a rid");
    let terminal_rid = rid_of(terminal_req).expect("terminal ping carries a rid");
    assert_eq!(
        start_rid, terminal_rid,
        "start and terminal pings must share the generated rid"
    );
    assert!(
        start_rid.starts_with('r'),
        "an auto-generated rid has the r-prefix: {start_rid}"
    );
}

#[tokio::test]
async fn run_with_preserves_supplied_rid_on_start_and_terminal() {
    let server = MockServer::start().await;
    // Both mocks require rid=nightly and .expect(1): the terminal AND the start
    // ping must each carry the caller's rid exactly once.
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .and(query_param("rid", "nightly"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(start_path()))
        .and(query_param("rid", "nightly"))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    job.run_with(
        || async { Ok::<(), String>(()) },
        PingOptions::new().rid("nightly"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn run_with_start_ping_false_skips_the_start_ping() {
    let server = MockServer::start().await;
    // Only the terminal success ping is expected; no mock for /start.
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    job.run_with(
        || async { Ok::<(), String>(()) },
        PingOptions::new().start_ping(false),
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|r| r.url.path() != start_path()),
        "no start ping should be sent when start_ping(false)"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == ping_path())
            .count(),
        1
    );
}

#[tokio::test]
async fn start_ping_failure_does_not_affect_closure_or_terminal_ping() {
    let server = MockServer::start().await;
    // The start ping always fails...
    Mock::given(method("GET"))
        .and(path(start_path()))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    // ...but the terminal success ping still succeeds.
    Mock::given(method("GET"))
        .and(path(ping_path()))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let value: u64 = job
        .run(|| async { Ok::<u64, String>(123) })
        .await
        .expect("run returns the closure value despite a start-ping failure");
    assert_eq!(value, 123);

    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().any(|r| r.url.path() == start_path()),
        "the start ping was attempted"
    );
    assert!(
        requests.iter().any(|r| r.url.path() == ping_path()),
        "the terminal ping was still sent after the start ping failed"
    );
}

// --- usage errors -----------------------------------------------------------

#[tokio::test]
async fn oversized_payload_returns_err_without_a_network_call() {
    let server = MockServer::start().await;
    // Mounting nothing: assert the server never saw a request.

    let big = "x".repeat(11_000);
    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let outcome = job
        .success_with(PingOptions::new().payload(json!({ "blob": big })))
        .await;

    // Usage errors are LOUD: returned as Err, not inside a PingResult.
    assert!(matches!(outcome, Err(Error::PayloadTooLarge { .. })));

    let requests = server.received_requests().await.unwrap();
    assert!(requests.is_empty(), "no HTTP request should be made");
}

#[tokio::test]
async fn empty_token_is_rejected() {
    let client = Client::builder()
        .base_url("https://example.com/api")
        .build()
        .unwrap();
    assert!(matches!(client.register_job(""), Err(Error::InvalidToken)));
}

#[tokio::test]
async fn whitespace_token_is_rejected() {
    let client = Client::builder()
        .base_url("https://example.com/api")
        .build()
        .unwrap();
    assert!(matches!(
        client.register_job("has space"),
        Err(Error::InvalidToken)
    ));
    assert!(matches!(
        client.register_job("tab\tinside"),
        Err(Error::InvalidToken)
    ));
}
