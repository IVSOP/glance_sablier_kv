use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use serde::Serialize;
use tower::ServiceExt;

#[derive(Serialize)]
struct Instance<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct Event<'a> {
    event: &'a str,
    instance: Instance<'a>,
    timestamp: &'a str,
}

fn event(name: &str, event: &str, timestamp: &str) -> String {
    serde_json::to_string(&Event {
        event,
        instance: Instance { name },
        timestamp,
    })
    .unwrap()
}

async fn post(app: &Router, payload: impl Into<Body>) -> Response {
    app.clone()
        .oneshot(
            Request::post("/webhooks/sablier")
                .header(header::CONTENT_TYPE, "application/json")
                .body(payload.into())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get(app: &Router, name: &str) -> Response {
    app.clone()
        .oneshot(
            Request::get(format!("/containers/{name}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_state(app: &Router, name: &str, expected: &str) {
    let response = get(app, name).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        expected
    );
}

#[tokio::test]
async fn health_succeeds_before_any_webhooks_arrive() {
    let response = sablier_mini_kv::app()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "ok");
}

#[tokio::test]
async fn unknown_container_returns_404() {
    let response = get(&sablier_mini_kv::app(), "unknown").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn records_lifecycle_events_and_keeps_containers_independent() {
    let app = sablier_mini_kv::app();
    for (state, timestamp) in [
        ("started", "2026-11-15T10:00:00Z"),
        ("paused", "2026-11-15T10:00:01Z"),
        ("stopped", "2026-11-15T10:00:02Z"),
    ] {
        let response = post(&app, event("nginx", state, timestamp)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        assert_state(&app, "nginx", state).await;
        assert_eq!(get(&app, "other").await.status(), StatusCode::NOT_FOUND);
    }
    post(&app, event("other", "started", "2026-11-15T10:00:03Z")).await;
    assert_state(&app, "nginx", "stopped").await;
    assert_state(&app, "other", "started").await;
}

#[tokio::test]
async fn older_delivery_cannot_overwrite_newer_state() {
    let app = sablier_mini_kv::app();
    post(&app, event("nginx", "stopped", "2026-11-15T10:00:02Z")).await;
    // Compare instants, including subsecond precision and timezone offsets.
    let older = event("nginx", "started", "2026-11-15T11:00:01.999+01:00");
    assert_eq!(post(&app, older).await.status(), StatusCode::NO_CONTENT);
    assert_state(&app, "nginx", "stopped").await;
}

#[tokio::test]
async fn duplicate_and_equal_timestamp_deliveries_succeed() {
    let app = sablier_mini_kv::app();
    let payload = event("nginx", "started", "2026-11-15T10:00:00Z");
    for _ in 0..2 {
        assert_eq!(
            post(&app, payload.clone()).await.status(),
            StatusCode::NO_CONTENT
        );
    }
    assert_state(&app, "nginx", "started").await;
    post(&app, event("nginx", "stopped", "2026-11-15T10:00:00Z")).await;
    assert_state(&app, "nginx", "stopped").await;
}

#[tokio::test]
async fn invalid_payloads_do_not_change_existing_state() {
    let app = sablier_mini_kv::app();
    post(&app, event("nginx", "started", "2026-11-15T10:00:00Z")).await;
    for payload in [
        event("nginx", "invented", "2026-11-15T10:00:01Z"),
        event("nginx", "stopped", "not-a-timestamp"),
        event("   ", "stopped", "2026-11-15T10:00:01Z"),
        r#"{"event":"stopped","instance":{"name":"nginx"}}"#.to_owned(),
        r#"{"event":"stopped","timestamp":"2026-11-15T10:00:01Z"}"#.to_owned(),
    ] {
        assert_eq!(
            post(&app, payload).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_state(&app, "nginx", "started").await;
    }
}

#[tokio::test]
async fn accepts_additional_fields_and_decodes_container_names() {
    let app = sablier_mini_kv::app();
    let payload = r#"{
        "event": "started",
        "instance": {"name": "my container/one", "id": "future-id"},
        "timestamp": "2026-11-15T10:00:00Z",
        "future_field": {"anything": true}
    }"#;
    assert_eq!(post(&app, payload).await.status(), StatusCode::NO_CONTENT);
    assert_state(&app, "my%20container%2Fone", "started").await;
}

#[tokio::test]
async fn rejects_malformed_non_json_and_oversized_requests() {
    let app = sablier_mini_kv::app();
    for (content_type, body, expected) in [
        ("application/json", "{".to_owned(), StatusCode::BAD_REQUEST),
        (
            "text/plain",
            "{}".to_owned(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/json",
            " ".repeat(16 * 1024 + 1),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/webhooks/sablier")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn a_fresh_service_has_no_previous_data() {
    let app = sablier_mini_kv::app();
    post(&app, event("nginx", "started", "2026-11-15T10:00:00Z")).await;
    assert_eq!(
        get(&sablier_mini_kv::app(), "nginx").await.status(),
        StatusCode::NOT_FOUND
    );
}
