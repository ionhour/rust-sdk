//! A stray `IONHOUR_API_KEY` in the environment must never become an
//! `Authorization` header: heartbeat pings authenticate by job token alone
//! until protected jobs ship (spec 027).
//!
//! This is its own integration-test binary on purpose. `std::env::set_var` is
//! process-global and races with concurrent `getenv` calls from other tests, so
//! the only test in this process is the one that sets the variable.

use ionhour::Client;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn env_api_key_is_ignored_and_no_authorization_header_is_sent() {
    std::env::set_var("IONHOUR_API_KEY", "ionh_stray_env_key");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::builder()
        .base_url(format!("{}/api", server.uri()))
        .build()
        .expect("client builds");
    let result = client.register_job("tok").unwrap().success().await.unwrap();
    assert!(result.ok);

    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("authorization").is_none());
}
