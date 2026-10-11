//! Slug mode: `register_job_by_slug` pings `{api_base}/signals/s/{slug}` on the
//! API host with `Authorization: Bearer <api_key>`. Token pings stay
//! credential-free. Env-driven configuration lives in `slug_env.rs` (its own
//! process, because `std::env::set_var` is process-global).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ionhour::{Client, Error, PingOptions, SlugJobOptions};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "ionh_rust_SECRETSECRETSECRET";
const SECRET_TAIL: &str = "SECRETSECRETSECRET";

/// What the `on_error` hook saw: the context slug, the fail flag, and the
/// rendered error and context text.
type HookSeen = Arc<Mutex<Vec<(Option<String>, bool, String)>>>;

/// Unroutable signal base: token pings must never reach the API mock, and slug
/// pings must never go here.
fn dead_base() -> String {
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    format!("http://{addr}/api")
}

fn slug_client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(dead_base())
        .api_base_url(format!("{}/api", server.uri()))
        .api_key(API_KEY)
        .retries(3)
        .backoff(vec![Duration::ZERO; 3])
        .build()
        .expect("client builds")
}

fn ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" }))
}

fn bearer() -> String {
    format!("Bearer {API_KEY}")
}

#[tokio::test]
async fn success_pings_api_host_with_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/signals/s/nightly-report"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;

    let job = slug_client(&server)
        .register_job_by_slug("nightly-report")
        .unwrap();
    let result = job.success().await.unwrap();

    assert!(result.ok);
    assert_eq!(result.status, Some(200));
    assert!(!result.created);
    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].url.query_pairs().all(|(k, _)| k != "create"));
}

#[tokio::test]
async fn fail_start_and_payload_routes() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl_job/fail"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/signals/s/etl_job/start"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/signals/s/etl_job"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;

    let job = slug_client(&server)
        .register_job_by_slug("etl_job")
        .unwrap();
    assert!(job.fail().await.unwrap().ok);
    assert!(job.start().await.unwrap().ok);
    let posted = job
        .success_with(PingOptions::new().payload(json!({ "rows": 3 })))
        .await
        .unwrap();
    assert!(posted.ok);
}

#[tokio::test]
async fn create_adds_query_param_alongside_telemetry() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl/fail"))
        .and(query_param("create", "1"))
        .and(query_param("rid", "r1"))
        .and(query_param("duration", "42"))
        .and(query_param("host", "web-1"))
        .and(query_param("env", "prod"))
        .and(query_param("exit_code", "2"))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;

    let job = slug_client(&server)
        .register_job_by_slug_with("etl", SlugJobOptions::new().create(true))
        .unwrap();
    let result = job
        .fail_with(
            PingOptions::new()
                .rid("r1")
                .duration_ms(42)
                .host("web-1")
                .env("prod")
                .exit_code(2),
        )
        .await
        .unwrap();
    assert!(result.ok);
}

#[tokio::test]
async fn created_from_201() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({ "message": "OK", "created": true })),
        )
        .mount(&server)
        .await;

    let job = slug_client(&server)
        .register_job_by_slug_with("etl", SlugJobOptions::new().create(true))
        .unwrap();
    let result = job.success().await.unwrap();
    assert!(result.ok && result.created);
    assert_eq!(result.status, Some(201));
}

#[tokio::test]
async fn created_from_body_on_200() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "message": "OK", "created": true })),
        )
        .mount(&server)
        .await;

    let job = slug_client(&server).register_job_by_slug("etl").unwrap();
    assert!(job.success().await.unwrap().created);
}

#[tokio::test]
async fn token_pings_never_send_authorization_with_api_key_configured() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/ping/tok"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "created": true })))
        .mount(&server)
        .await;
    Mock::given(path("/api/signals/ping/tok/fail"))
        .respond_with(ok())
        .mount(&server)
        .await;
    Mock::given(path("/api/signals/ping/tok/start"))
        .respond_with(ok())
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .api_key(API_KEY)
        .build()
        .unwrap();
    let job = client.register_job("tok").unwrap();
    let result = job.success().await.unwrap();
    job.fail().await.unwrap();
    job.start().await.unwrap();
    job.success_with(PingOptions::new().payload(json!({ "a": 1 })))
        .await
        .unwrap();

    assert!(!result.created, "token pings never report created");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests {
        assert!(request.headers.get("authorization").is_none());
    }
}

#[test]
fn rejects_invalid_slugs() {
    let client = Client::builder().api_key(API_KEY).build().unwrap();
    let too_long = "a".repeat(65);
    for slug in ["", "Nightly", too_long.as_str(), "a/b", "a b", "a.b", "ü"] {
        assert!(
            matches!(
                client.register_job_by_slug(slug),
                Err(Error::InvalidSlug { .. })
            ),
            "slug {slug:?} should be rejected"
        );
    }
    let max = "a".repeat(64);
    for slug in ["a", max.as_str(), "nightly_job-2"] {
        assert!(client.register_job_by_slug(slug).is_ok(), "{slug:?}");
    }
}

#[test]
fn missing_api_key_is_a_usage_error() {
    let client = Client::builder().api_key("   ").build().unwrap();
    let err = client.register_job_by_slug("etl").unwrap_err();
    assert!(matches!(err, Error::MissingApiKey));
    assert!(err.to_string().contains("IONHOUR_API_KEY"));
}

#[test]
fn malformed_api_key_is_rejected_without_echoing_it() {
    let client = Client::builder()
        .api_key("ionh_abc\r\nX-Injected: 1")
        .build()
        .unwrap();
    let err = client.register_job_by_slug("etl").unwrap_err();
    assert!(matches!(err, Error::InvalidApiKey));
    assert!(!err.to_string().contains("ionh_abc"));
}

#[test]
fn token_jobs_need_no_api_key() {
    assert!(Client::builder()
        .build()
        .unwrap()
        .register_job("tok")
        .is_ok());
}

#[tokio::test]
async fn retries_429_and_5xx() {
    for status in [429u16, 502] {
        let server = MockServer::start().await;
        Mock::given(path("/api/signals/s/etl"))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/signals/s/etl"))
            .respond_with(ok())
            .mount(&server)
            .await;

        let job = slug_client(&server).register_job_by_slug("etl").unwrap();
        let result = job.success().await.unwrap();
        assert!(result.ok, "status {status} should be retried");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn never_retries_envelope_4xx_and_surfaces_code() {
    let cases = [
        (400u16, "VALIDATION_FAILED"),
        (401, "MISSING_BEARER"),
        (401, "SERVICE_KEY_INACTIVE"),
        (403, "KEY_SCOPE_DENIED"),
        (403, "INSUFFICIENT_ROLE"),
        (403, "READ_ONLY_KEY"),
        (403, "FEATURE_DISABLED"),
        (404, "NOT_FOUND"),
    ];
    for (status, code) in cases {
        let server = MockServer::start().await;
        Mock::given(path("/api/signals/s/etl"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "statusCode": status, "error": "x", "code": code, "message": "nope"
            })))
            .mount(&server)
            .await;

        let job = slug_client(&server)
            .register_job_by_slug_with("etl", SlugJobOptions::new().create(true))
            .unwrap();
        let result = job.success().await.unwrap();

        assert!(!result.ok);
        assert_eq!(result.status, Some(status));
        assert_eq!(server.received_requests().await.unwrap().len(), 1, "{code}");
        let err = result.error.expect("error present");
        assert!(err.to_string().contains(code), "{err}");
        match err {
            Error::Http {
                status: s, code: c, ..
            } => {
                assert_eq!(s, status);
                assert_eq!(c.as_deref(), Some(code));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn ignores_malformed_error_code() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({ "code": format!("bad {API_KEY}") })),
        )
        .mount(&server)
        .await;

    let job = slug_client(&server).register_job_by_slug("etl").unwrap();
    let err = job.success().await.unwrap().error.unwrap();
    assert!(matches!(
        err,
        Error::Http {
            status: 403,
            code: None,
            ..
        }
    ));
    assert!(!err.to_string().contains(SECRET_TAIL));
}

#[tokio::test]
async fn on_error_context_names_the_slug_and_transport_text_has_no_key() {
    let seen: HookSeen = Arc::default();
    let sink = seen.clone();
    let client = Client::builder()
        .base_url(dead_base())
        .api_base_url(dead_base())
        .api_key(API_KEY)
        .retries(1)
        .on_error(move |err, ctx| {
            sink.lock().unwrap().push((
                ctx.slug().map(str::to_string),
                ctx.is_fail(),
                format!("{err} {err:?} {ctx:?}"),
            ));
        })
        .build()
        .unwrap();

    let job = client.register_job_by_slug("etl").unwrap();
    let result = job.fail().await.unwrap();

    assert!(!result.ok);
    let shown = format!("{:?}", result.error);
    assert!(!shown.contains(SECRET_TAIL), "{shown}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some("etl"));
    assert!(seen[0].1);
    assert!(!seen[0].2.contains(SECRET_TAIL), "{}", seen[0].2);
}

#[test]
fn debug_output_never_shows_the_api_key() {
    let builder = Client::builder().api_key(API_KEY);
    assert!(!format!("{builder:?}").contains(SECRET_TAIL));
    let client = builder.build().unwrap();
    assert!(!format!("{client:?}").contains(SECRET_TAIL));
    let job = client
        .register_job_by_slug_with("etl", SlugJobOptions::new().create(true))
        .unwrap();
    let rendered = format!("{job:?}");
    assert!(!rendered.contains(SECRET_TAIL), "{rendered}");
    assert!(rendered.contains("etl"), "{rendered}");
}

#[tokio::test]
async fn run_sends_bearer_on_start_and_terminal_pings() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/s/etl/start"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/api/signals/s/etl"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ok())
        .expect(1)
        .mount(&server)
        .await;

    let job = slug_client(&server).register_job_by_slug("etl").unwrap();
    let value: Result<u8, std::io::Error> = job.run(|| async { Ok(7) }).await;
    assert_eq!(value.unwrap(), 7);
}

#[test]
fn invalid_slug_error_never_echoes_the_full_value() {
    // A caller that swaps the arguments passes the key as the slug.
    let client = Client::builder().api_key("ionh_ok").build().unwrap();
    let err = client.register_job_by_slug(API_KEY).unwrap_err();
    assert!(matches!(err, Error::InvalidSlug { .. }));
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(SECRET_TAIL), "{shown}");
    assert!(shown.contains("ionh_rus…"), "{shown}");
}

#[test]
fn explicit_malformed_api_base_url_fails_at_build() {
    let err = Client::builder()
        .api_base_url("not a url")
        .build()
        .unwrap_err();
    assert!(matches!(err, Error::InvalidConfig(_)));
}

#[tokio::test]
async fn token_ping_403_envelope_yields_plain_status() {
    let server = MockServer::start().await;
    Mock::given(path("/api/signals/ping/tok"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "statusCode": 403, "error": "x", "code": "KEY_SCOPE_DENIED", "message": "no"
        })))
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .api_key(API_KEY)
        .build()
        .unwrap();
    let err = client
        .register_job("tok")
        .unwrap()
        .success()
        .await
        .unwrap()
        .error
        .unwrap();
    assert!(matches!(
        err,
        Error::Http {
            status: 403,
            code: None,
            ..
        }
    ));
    assert_eq!(err.to_string(), "ionhour responded with HTTP 403");
}

#[tokio::test]
async fn reads_at_most_4_kib_of_a_slug_body() {
    // The code sits past the 4 KiB cap, so it must never be seen.
    let server = MockServer::start().await;
    let body = format!(
        r#"{{"pad":"{}","code":"KEY_SCOPE_DENIED"}}"#,
        "x".repeat(5_000)
    );
    Mock::given(path("/api/signals/s/etl"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let job = slug_client(&server).register_job_by_slug("etl").unwrap();
    let err = job.success().await.unwrap().error.unwrap();
    assert!(matches!(
        err,
        Error::Http {
            status: 403,
            code: None,
            ..
        }
    ));
}
