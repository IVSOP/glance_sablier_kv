use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{Json, Router, extract::State, http::header, response::IntoResponse, routing::post};
use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize, de};
use tokio::{
    sync::Semaphore,
    time::{Instant as TokioInstant, timeout_at},
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
// Glance's Custom API client waits at most five seconds for the entire response.
const BATCH_TIMEOUT: Duration = Duration::from_millis(4500);
const MAX_CONCURRENT_CHECKS: usize = 20;

#[derive(Deserialize)]
struct CheckRequest {
    sites: Vec<Site>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Site {
    title: String,
    url: String,
    check_url: Option<String>,
    icon: Option<String>,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_timeout")]
    timeout: Duration,
    #[serde(default)]
    allow_insecure: bool,
    #[serde(default)]
    alt_status_codes: Vec<u16>,
    basic_auth: Option<BasicAuth>,
}

#[derive(Deserialize)]
struct BasicAuth {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

#[derive(Serialize)]
struct CheckResponse {
    sites: Vec<SiteHealth>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct SiteHealth {
    title: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(flatten)]
    status: HealthStatus,
    response_time_ms: u128,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum HealthStatus {
    Ok { code: u16 },
    HttpError { code: u16 },
    TimedOut,
    Unreachable { error: String },
}

#[derive(Clone)]
struct Checker {
    secure: Client,
    insecure: Client,
    concurrent_checks: Arc<Semaphore>,
}

pub(super) fn router() -> Result<Router, reqwest::Error> {
    let checker = Checker {
        secure: Client::builder().build()?,
        insecure: Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()?,
        concurrent_checks: Arc::new(Semaphore::new(MAX_CONCURRENT_CHECKS)),
    };
    Ok(Router::new()
        .route("/checks", post(check_sites))
        .with_state(checker))
}

async fn check_sites(
    State(checker): State<Checker>,
    Json(request): Json<CheckRequest>,
) -> impl IntoResponse {
    let deadline = TokioInstant::now() + BATCH_TIMEOUT;
    let sites = join_all(
        request
            .sites
            .into_iter()
            .map(|site| checker.check(site, deadline)),
    )
    .await;
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(CheckResponse { sites }),
    )
}

impl Checker {
    async fn check(&self, site: Site, deadline: TokioInstant) -> SiteHealth {
        let start = Instant::now();
        let status = timeout_at(deadline, async {
            let _permit = self
                .concurrent_checks
                .acquire()
                .await
                .expect("the check semaphore is never closed");
            self.probe(&site).await
        })
        .await
        .unwrap_or(HealthStatus::TimedOut);

        SiteHealth {
            title: site.title,
            url: site.url,
            icon: site.icon,
            status,
            response_time_ms: start.elapsed().as_millis(),
        }
    }

    async fn probe(&self, site: &Site) -> HealthStatus {
        let client = if site.allow_insecure {
            &self.insecure
        } else {
            &self.secure
        };
        let check_url = site
            .check_url
            .as_deref()
            .filter(|url| !url.is_empty())
            .unwrap_or(&site.url);
        let mut request = client.get(check_url).timeout(site.timeout);
        if let Some(auth) = &site.basic_auth
            && (!auth.username.is_empty() || !auth.password.is_empty())
        {
            request = request.basic_auth(&auth.username, Some(&auth.password));
        }

        match request.send().await {
            Ok(response) => {
                let code = response.status().as_u16();
                if code == 200 || site.alt_status_codes.contains(&code) {
                    HealthStatus::Ok { code }
                } else {
                    HealthStatus::HttpError { code }
                }
            }
            Err(error) if error.is_timeout() => HealthStatus::TimedOut,
            Err(error) => HealthStatus::Unreachable {
                error: error.without_url().to_string(),
            },
        }
    }
}

fn default_timeout() -> Duration {
    DEFAULT_TIMEOUT
}

fn deserialize_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let value = String::deserialize(deserializer)?;
    let invalid = || de::Error::custom("timeout must be an integer followed by s, m, h, or d");
    let seconds_per_unit = match value.as_bytes().last() {
        Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 60 * 60,
        Some(b'd') => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    let amount = &value[..value.len() - 1];
    if !amount.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let amount = amount.parse::<u64>().map_err(|_| invalid())?;
    if amount == 0 {
        return Ok(DEFAULT_TIMEOUT);
    }
    Ok(Duration::from_secs(amount.saturating_mul(seconds_per_unit)).min(CLIENT_TIMEOUT))
}

#[cfg(test)]
mod tests;
