use std::{
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
    response::{Redirect, Response},
    routing::get,
};
use serde::Deserialize;
use tokio::net::TcpListener;
use tower::ServiceExt;

use super::{CheckRequest, DEFAULT_TIMEOUT};

#[derive(Deserialize)]
struct Results {
    sites: Vec<ResultSite>,
}

#[derive(Debug, Deserialize)]
struct ResultSite {
    title: String,
    url: String,
    icon: Option<String>,
    status: String,
    code: Option<u16>,
}

async fn post_checks(app: &Router, body: impl Into<Body>) -> Response {
    app.clone()
        .oneshot(
            Request::post("/checks")
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.into())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_sites(response: Response) -> Vec<ResultSite> {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    serde_json::from_slice::<Results>(&body).unwrap().sites
}

#[test]
fn parses_explicit_and_default_timeouts() {
    let request: CheckRequest = serde_json::from_str(
        r#"{"sites": [
            {"title": "Default", "url": "http://localhost"},
            {"title": "Zero", "url": "http://localhost", "timeout": "0s"},
            {"title": "Glance", "url": "http://localhost", "timeout": "2s"},
            {"title": "Milliseconds", "url": "http://localhost", "timeout": "250ms"},
            {"title": "Long", "url": "http://localhost", "timeout": "5m"}
        ]}"#,
    )
    .unwrap();

    let timeouts: Vec<_> = request.sites.into_iter().map(|site| site.timeout).collect();
    assert_eq!(
        timeouts,
        [
            DEFAULT_TIMEOUT,
            DEFAULT_TIMEOUT,
            Duration::from_secs(2),
            Duration::from_millis(250),
            Duration::from_secs(300),
        ],
    );
}

#[tokio::test]
async fn checks_sites_only_when_requested_and_returns_fresh_results() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let code = Arc::new(AtomicU16::new(204));
    let requests = Arc::new(AtomicUsize::new(0));
    let upstream = Router::new()
        .route("/redirect", get(|| async { Redirect::to("/health") }))
        .route(
            "/health",
            get({
                let code = Arc::clone(&code);
                let requests = Arc::clone(&requests);
                move |headers: HeaderMap| {
                    let code = Arc::clone(&code);
                    let requests = Arc::clone(&requests);
                    async move {
                        requests.fetch_add(1, Ordering::Relaxed);
                        if headers
                            .get(header::AUTHORIZATION)
                            .is_some_and(|value| value == "Basic dXNlcjpwYXNz")
                        {
                            StatusCode::from_u16(code.load(Ordering::Relaxed)).unwrap()
                        } else {
                            StatusCode::UNAUTHORIZED
                        }
                    }
                }
            }),
        )
        .route("/fallback", get(|| async { StatusCode::OK }));
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let app = crate::app().unwrap();
    assert_eq!(requests.load(Ordering::Relaxed), 0);

    let body = format!(
        r#"{{
            "sites": [{{
                "title": "Protected",
                "url": "https://example.com/",
                "icon": "/assets/example.svg",
                "check-url": "http://{address}/redirect",
                "alt-status-codes": [204],
                "basic-auth": {{"username": "user", "password": "pass"}}
            }}, {{
                "title": "Fallback",
                "url": "http://{address}/fallback"
            }}]
        }}"#
    );
    let sites = response_sites(post_checks(&app, body.clone()).await).await;
    assert_eq!(sites[0].title, "Protected");
    assert_eq!(sites[0].url, "https://example.com/");
    assert_eq!(sites[0].icon.as_deref(), Some("/assets/example.svg"));
    assert_eq!(sites[0].status, "ok");
    assert_eq!(sites[0].code, Some(204));
    assert_eq!(sites[1].title, "Fallback");
    assert_eq!(sites[1].status, "ok");
    assert_eq!(sites[1].code, Some(200));
    assert_eq!(requests.load(Ordering::Relaxed), 1);

    code.store(503, Ordering::Relaxed);
    let sites = response_sites(post_checks(&app, body).await).await;
    assert_eq!(sites[0].status, "http-error");
    assert_eq!(sites[0].code, Some(503));
    assert_eq!(requests.load(Ordering::Relaxed), 2);

    server.abort();
}

#[tokio::test]
async fn reports_transport_errors_and_timeouts_alongside_other_sites() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new().route(
        "/slow",
        get(|| async {
            std::future::pending::<()>().await;
            StatusCode::OK
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let body = format!(
        r#"{{
            "sites": [
                {{"title": "Slow", "url": "http://{address}/slow", "timeout": "1s"}},
                {{"title": "Invalid URL", "url": "not an HTTP URL"}}
            ]
        }}"#
    );

    let sites = response_sites(post_checks(&crate::app().unwrap(), body).await).await;
    assert_eq!(sites[0].status, "timed-out");
    assert_eq!(sites[1].status, "unreachable");
    assert!(sites.iter().all(|site| site.code.is_none()));

    server.abort();
}

#[tokio::test]
async fn reports_sleeping_only_for_failed_probes_with_a_stop_event() {
    let app = crate::app().unwrap();
    for (name, event) in [
        ("stopped", "stopped"),
        ("started", "started"),
        ("paused", "paused"),
    ] {
        let body = format!(
            r#"{{"event":"{event}","instance":{{"name":"{name}"}},"timestamp":"2026-11-20T00:00:00Z"}}"#
        );
        let response = app
            .clone()
            .oneshot(
                Request::post("/webhooks/sablier")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let unreachable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = unreachable.local_addr().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    drop(unreachable);
    let available = listener.local_addr().unwrap();
    let upstream = Router::new()
        .route("/ok", get(|| async { StatusCode::OK }))
        .route(
            "/error",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let body = format!(
        r#"{{"sites":[
            {{"title":"Stopped","url":"http://{unavailable}","sablier-container":"stopped"}},
            {{"title":"Paused","url":"http://{unavailable}","sablier-container":"paused"}},
            {{"title":"Started","url":"http://{unavailable}","sablier-container":"started"}},
            {{"title":"Unknown","url":"http://{unavailable}","sablier-container":"unknown"}},
            {{"title":"Unmapped","url":"http://{unavailable}"}},
            {{"title":"Reachable","url":"http://{available}/ok","sablier-container":"stopped"}},
            {{"title":"Failure","url":"http://{available}/error","sablier-container":"stopped"}}
        ]}}"#
    );

    let sites = response_sites(post_checks(&app, body).await).await;
    let statuses: Vec<_> = sites.iter().map(|site| site.status.as_str()).collect();
    assert_eq!(
        statuses,
        [
            "sleeping",
            "sleeping",
            "unreachable",
            "unreachable",
            "unreachable",
            "ok",
            "http-error"
        ]
    );
    assert_eq!(sites[5].code, Some(200));
    assert_eq!(sites[6].code, Some(500));
    server.abort();
}
