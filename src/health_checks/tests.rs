use std::{
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
    response::Redirect,
    routing::get,
};
use tokio::net::TcpListener;

use super::{HealthChecks, HealthStatus, Snapshot, config};

#[test]
fn reads_glance_page_settings_and_expands_monitor_variables() {
    let monitors = config::parse(
        r#"
- name: Home
  columns:
    - size: full
      widgets:
        - type: dns-stats
          password: ${UNRELATED_PASSWORD}
        - type: split-column
          widgets:
            - type: monitor
              cache: ${CACHE}
              sites:
                - title: Example
                  url: https://example.com
                  check-url: http://${HOST}/${PATH}
                  timeout: ${TIMEOUT}
                  alt-status-codes: [401, 204]
                  allow-insecure: true
                  basic-auth:
                    username: user
                    password: "${PASSWORD}"
                - title: Uses display URL
                  url: https://example.com/other
"#,
        |name| {
            match name {
                "CACHE" => Some("1m"),
                "HOST" => Some("service"),
                "PATH" => Some("health"),
                "TIMEOUT" => Some("10s"),
                "PASSWORD" => Some("a: \"quoted\" password"),
                _ => None,
            }
            .map(str::to_owned)
        },
    )
    .unwrap();

    assert_eq!(monitors.len(), 1);
    assert_eq!(monitors[0].cache, Duration::from_secs(60));
    let sites = &monitors[0].sites;
    assert_eq!(sites[0].check_url.as_str(), "http://service/health");
    assert_eq!(sites[0].timeout, Duration::from_secs(5));
    assert_eq!(sites[0].alt_status_codes, [401, 204]);
    assert!(sites[0].allow_insecure);
    assert_eq!(
        sites[0].basic_auth.as_ref().unwrap().password,
        "a: \"quoted\" password"
    );
    assert_eq!(sites[1].check_url.as_str(), "https://example.com/other");
    assert_eq!(sites[1].timeout, Duration::from_secs(3));
}

#[tokio::test]
async fn polls_and_caches_http_results() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let code = Arc::new(AtomicU16::new(204));
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
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
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                StatusCode::OK
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut monitors = config::parse(
        &format!(
            r#"
cache: 1s
sites:
  - title: Protected
    url: http://{address}/unused
    check-url: http://{address}/redirect
    alt-status-codes: [204]
    basic-auth:
      username: user
      password: pass
  - title: Slow
    url: http://{address}/slow
"#
        ),
        |_| None,
    )
    .unwrap();
    monitors[0].sites[1].timeout = Duration::from_millis(75);
    let checks = Arc::new(HealthChecks::new(monitors).unwrap());
    assert!(checks.snapshots().await.is_empty());
    let worker = tokio::spawn({
        let checks = Arc::clone(&checks);
        async move { checks.run().await }
    });

    let first = wait_for_snapshot(&checks, 204).await;
    assert_eq!(
        first.sites[0].status,
        HealthStatus::Http {
            code: 204,
            ok: true
        }
    );
    assert_eq!(first.sites[1].status, HealthStatus::TimedOut);
    code.store(503, Ordering::Relaxed);
    assert_eq!(checks.snapshots().await[0].checked_at, first.checked_at);
    assert_eq!(requests.load(Ordering::Relaxed), 1);

    let next = wait_for_snapshot(&checks, 503).await;
    assert_eq!(
        next.sites[0].status,
        HealthStatus::Http {
            code: 503,
            ok: false
        }
    );
    assert_eq!(requests.load(Ordering::Relaxed), 2);

    worker.abort();
    server.abort();
}

async fn wait_for_snapshot(checks: &HealthChecks, expected: u16) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = checks.snapshots().await.pop()
                && matches!(snapshot.sites[0].status, HealthStatus::Http { code, .. } if code == expected)
            {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("expected a completed snapshot with the requested status")
}
