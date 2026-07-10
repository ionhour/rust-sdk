//! Integration tests for the Ionhour SDK against a mocked HTTP server
//! (`wiremock`). Covers the full required-test list from `sdks/CONTRACT.md`.
//!
//! Relies on the `test-util` crate feature (enabled via the self-referential
//! dev-dependency in Cargo.toml) for `ClientBuilder::backoff`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ionhour::{Client, Error, PingOptions};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "job-token-abc";

fn ping_path() -> String {
    format!("/api/signals/ping/{TOKEN}")
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
    let result = job.success(PingOptions::new()).await.unwrap();

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
        .success(PingOptions::new().payload(payload))
        .await
        .unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
}

#[tokio::test]
async fn fail_uses_fail_suffix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/signals/ping/{TOKEN}/fail")))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let result = job.fail(PingOptions::new()).await.unwrap();

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
        .success(PingOptions::new().rid("run-1").duration_ms(1500))
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
    job.success(PingOptions::new()).await.unwrap();

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
    job.success(PingOptions::new()).await.unwrap();

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
    job.success(PingOptions::new()).await.unwrap();

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
    job.success(PingOptions::new()).await.unwrap();

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
    job.success(PingOptions::new()).await.unwrap();

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
    let result = job.success(PingOptions::new()).await.unwrap();

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
    let result = job.success(PingOptions::new()).await.unwrap();

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
    let result = job.success(PingOptions::new()).await.unwrap();

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
    let result = job.success(PingOptions::new()).await.unwrap();

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
    let result = job.success(PingOptions::new()).await.unwrap();

    assert!(!result.ok);
    assert!(result.status.is_none());
    assert!(matches!(result.error, Some(Error::Transport(_))));
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
    let value: u64 = job
        .run(
            || async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok::<u64, String>(7)
            },
            PingOptions::new(),
        )
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
        .and(path(format!("/api/signals/ping/{TOKEN}/fail")))
        .respond_with(ok_body())
        .expect(1)
        .mount(&server)
        .await;

    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let outcome: Result<(), String> = job
        .run(
            || async { Err::<(), String>("kaboom".to_string()) },
            PingOptions::new(),
        )
        .await;

    assert_eq!(outcome, Err("kaboom".to_string()));

    let requests = server.received_requests().await.unwrap();
    let request = requests.last().unwrap();
    assert!(request.url.path().ends_with("/fail"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["error"], json!("kaboom"));
}

// --- usage errors -----------------------------------------------------------

#[tokio::test]
async fn oversized_payload_returns_err_without_a_network_call() {
    let server = MockServer::start().await;
    // Mounting nothing: assert the server never saw a request.

    let big = "x".repeat(11_000);
    let job = client_for(&server, None).register_job(TOKEN).unwrap();
    let outcome = job
        .success(PingOptions::new().payload(json!({ "blob": big })))
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
