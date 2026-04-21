use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shortlink_service::{ServiceConfig, app_from_config};
use std::time::Duration;
use tower::ServiceExt;

fn test_config(default_ttl: Duration) -> ServiceConfig {
    ServiceConfig::new(128, default_ttl, "http://short.test").expect("valid test config")
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body should collect")
        .to_bytes();

    serde_json::from_slice(&bytes).expect("response body should be json")
}

#[tokio::test]
async fn create_link_generates_alias() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/docs"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response_json(response).await;
    assert_eq!(body["url"], "https://example.com/docs");
    assert_eq!(
        body["short_url"],
        "http://short.test/".to_owned() + body["alias"].as_str().unwrap()
    );
    assert_eq!(body["hit_count"], 0);
    assert_eq!(body["alias"].as_str().unwrap().len(), 7);
}

#[tokio::test]
async fn create_link_with_custom_alias() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/blog",
                        "alias": "blog-home"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response_json(response).await;
    assert_eq!(body["alias"], "blog-home");
}

#[tokio::test]
async fn duplicate_custom_alias_returns_conflict() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    for expected_status in [StatusCode::CREATED, StatusCode::CONFLICT] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/links")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "url": "https://example.com/a",
                            "alias": "dup-alias"
                        })
                        .to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), expected_status);
    }
}

#[tokio::test]
async fn invalid_input_returns_bad_request() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let cases = [
        json!({ "url": "ftp://example.com/file" }),
        json!({ "url": "https://example.com/x", "alias": "no" }),
        json!({ "url": "https://example.com/x", "ttl_seconds": 0 }),
    ];

    for payload in cases {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/links")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("request should build"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn get_and_delete_link_lifecycle() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/state",
                        "alias": "stateful"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(create.status(), StatusCode::CREATED);

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/links/stateful")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(detail.status(), StatusCode::OK);

    let delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/links/stateful")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let missing = app
        .oneshot(
            Request::builder()
                .uri("/api/links/stateful")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redirect_returns_found() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/go",
                        "alias": "go-link"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(create.status(), StatusCode::CREATED);

    let redirect = app
        .oneshot(
            Request::builder()
                .uri("/go-link")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");

    assert_eq!(redirect.status(), StatusCode::FOUND);
    assert_eq!(
        redirect.headers().get(header::LOCATION).unwrap(),
        "https://example.com/go"
    );
}

#[tokio::test]
async fn ttl_expiry_returns_not_found() {
    let app = app_from_config(test_config(Duration::from_millis(25)));

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/ttl",
                        "alias": "ttl-link"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(create.status(), StatusCode::CREATED);

    tokio::time::sleep(Duration::from_millis(40)).await;

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/links/ttl-link")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);

    let redirect = app
        .oneshot(
            Request::builder()
                .uri("/ttl-link")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(redirect.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redirect_increments_hit_count() {
    let app = app_from_config(test_config(Duration::from_secs(60)));

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/links")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "url": "https://example.com/hits",
                        "alias": "hit-link"
                    })
                    .to_string(),
                ))
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(create.status(), StatusCode::CREATED);

    let redirect = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/hit-link")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(redirect.status(), StatusCode::FOUND);

    let detail = app
        .oneshot(
            Request::builder()
                .uri("/api/links/hit-link")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should succeed");
    assert_eq!(detail.status(), StatusCode::OK);

    let body = response_json(detail).await;
    assert_eq!(body["hit_count"], 1);
}
