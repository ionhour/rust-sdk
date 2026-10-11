//! A malformed `IONHOUR_API_BASE_URL` must never break token-only clients or
//! discard a valid `IONHOUR_BASE_URL`; it surfaces only when slug mode is used.
//! Its own test binary because `std::env::set_var` is process-global.

use ionhour::{Client, Error};
use serde_json::json;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn malformed_env_api_base_url_only_affects_slug_mode() {
    let server = MockServer::start().await;
    std::env::set_var("IONHOUR_BASE_URL", format!("{}/api", server.uri()));
    std::env::set_var("IONHOUR_API_BASE_URL", "not a url");
    std::env::remove_var("IONHOUR_API_KEY");

    Mock::given(path("/api/signals/ping/tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" })))
        .expect(1)
        .mount(&server)
        .await;

    // Token-only clients build and keep the valid IONHOUR_BASE_URL.
    assert!(Client::builder().build().is_ok());
    let client = Client::new();
    let result = client.register_job("tok").unwrap().success().await.unwrap();
    assert!(result.ok);

    // Slug mode reports the bad env value at registration.
    let keyed = Client::builder().api_key("ionh_key").build().unwrap();
    assert!(matches!(
        keyed.register_job_by_slug("etl"),
        Err(Error::InvalidConfig(_))
    ));
}
