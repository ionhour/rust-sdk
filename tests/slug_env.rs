//! Slug-mode env fallbacks: `IONHOUR_API_BASE_URL` and `IONHOUR_API_KEY`
//! (trimmed). Its own test binary because `std::env::set_var` is
//! process-global and races with other tests' `getenv`.

use ionhour::Client;
use serde_json::json;
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn api_base_url_and_trimmed_api_key_come_from_env() {
    let server = MockServer::start().await;
    std::env::set_var("IONHOUR_API_BASE_URL", format!("{}/api/", server.uri()));
    std::env::set_var("IONHOUR_API_KEY", "  ionh_env_key\n");

    Mock::given(path("/api/signals/s/etl"))
        .and(header("authorization", "Bearer ionh_env_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" })))
        .expect(1)
        .mount(&server)
        .await;

    let job = Client::new().register_job_by_slug("etl").unwrap();
    assert!(job.success().await.unwrap().ok);

    // An explicit key wins over the env (and is trimmed) ...
    Mock::given(path("/api/signals/s/explicit"))
        .and(header("authorization", "Bearer ionh_explicit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" })))
        .expect(1)
        .mount(&server)
        .await;
    let explicit = Client::builder()
        .api_key(" ionh_explicit ")
        .build()
        .unwrap();
    let job = explicit.register_job_by_slug("explicit").unwrap();
    assert!(job.success().await.unwrap().ok);

    // ... and a blank explicit key falls back to the env.
    Mock::given(path("/api/signals/s/blank"))
        .and(header("authorization", "Bearer ionh_env_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "OK" })))
        .expect(1)
        .mount(&server)
        .await;
    let blank = Client::builder().api_key("").build().unwrap();
    let job = blank.register_job_by_slug("blank").unwrap();
    assert!(job.success().await.unwrap().ok);
}
