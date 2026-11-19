pub mod health_checks;

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::RwLock;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

type Store = Arc<RwLock<HashMap<String, ContainerState>>>;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Event {
    Started,
    Stopped,
    Paused,
}

impl Event {
    fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::Paused => "paused",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Instance {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Webhook {
    event: Event,
    instance: Instance,
    timestamp: DateTime<Utc>,
}

struct ContainerState {
    event: Event,
    timestamp: DateTime<Utc>,
}

/// Build a service with its own empty, process-local store.
pub fn app() -> Router {
    let store: Store = Arc::default();

    Router::new()
        .route("/health", get(health))
        .route("/webhooks/sablier", post(record_event))
        .route("/containers/{name}", get(container_state))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(store)
}

async fn health() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], "ok")
}

async fn record_event(
    State(store): State<Store>,
    Json(webhook): Json<Webhook>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    if webhook.instance.name.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "instance.name must not be empty",
        ));
    }

    let mut states = store.write().await;
    if states
        .get(&webhook.instance.name)
        .is_some_and(|current| current.timestamp > webhook.timestamp)
    {
        tracing::debug!(container = %webhook.instance.name, "ignored older event");
        return Ok(StatusCode::NO_CONTENT);
    }

    tracing::info!(
        container = %webhook.instance.name,
        event = webhook.event.as_str(),
        timestamp = %webhook.timestamp,
        "recorded container state"
    );
    states.insert(
        webhook.instance.name,
        ContainerState {
            event: webhook.event,
            timestamp: webhook.timestamp,
        },
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn container_state(State(store): State<Store>, Path(name): Path<String>) -> Response {
    let states = store.read().await;
    let (status, body) = match states.get(&name) {
        Some(state) => (StatusCode::OK, state.event.as_str()),
        None => (StatusCode::NOT_FOUND, "container not found"),
    };
    (status, [(header::CACHE_CONTROL, "no-store")], body).into_response()
}
