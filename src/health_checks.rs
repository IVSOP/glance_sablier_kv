mod config;

use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant, SystemTime},
};

use futures_util::{StreamExt, future::join_all, stream};
use reqwest::Client;
use tokio::sync::RwLock;

use config::MonitorConfig;

const DEFAULT_CACHE_DURATION: Duration = Duration::from_secs(5 * 60);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_CHECKS: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthStatus {
    Http { code: u16, ok: bool },
    TimedOut,
    Unreachable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteHealth {
    pub title: String,
    pub status: HealthStatus,
    pub response_time: Duration,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub checked_at: SystemTime,
    pub sites: Vec<SiteHealth>,
}

pub struct HealthChecks {
    clients: Clients,
    monitors: Vec<Monitor>,
}

struct Clients {
    secure: Client,
    insecure: Client,
}

struct Monitor {
    config: MonitorConfig,
    snapshot: RwLock<Option<Snapshot>>,
}

impl HealthChecks {
    /// Load Glance Monitor settings and prepare their in-memory caches.
    pub fn load(path: &Path, env_file: Option<&Path>) -> Result<Self, Box<dyn Error>> {
        Ok(Self::new(config::load(path, env_file)?)?)
    }

    fn new(monitors: Vec<MonitorConfig>) -> Result<Self, reqwest::Error> {
        Ok(Self {
            clients: Clients {
                secure: Client::builder().build()?,
                insecure: Client::builder()
                    .tls_danger_accept_invalid_certs(true)
                    .build()?,
            },
            monitors: monitors
                .into_iter()
                .map(|config| Monitor {
                    config,
                    snapshot: RwLock::default(),
                })
                .collect(),
        })
    }

    /// Poll immediately, then refresh each monitor after its own cache lifetime.
    pub async fn run(&self) {
        join_all(self.monitors.iter().map(|monitor| async {
            loop {
                monitor.refresh(&self.clients).await;
                tokio::time::sleep(monitor.config.cache).await;
            }
        }))
        .await;
    }

    /// Return completed monitor snapshots without triggering any requests.
    pub async fn snapshots(&self) -> Vec<Snapshot> {
        let mut snapshots = Vec::with_capacity(self.monitors.len());
        for monitor in &self.monitors {
            snapshots.extend(monitor.snapshot.read().await.clone());
        }
        snapshots
    }
}

impl Monitor {
    async fn refresh(&self, clients: &Clients) {
        let checks: Vec<_> = self
            .config
            .sites
            .iter()
            .map(|site| clients.check(site))
            .collect();
        let sites = stream::iter(checks)
            .buffered(MAX_CONCURRENT_CHECKS)
            .collect()
            .await;

        *self.snapshot.write().await = Some(Snapshot {
            checked_at: SystemTime::now(),
            sites,
        });
        tracing::debug!(sites = self.config.sites.len(), "refreshed health checks");
    }
}

impl Clients {
    async fn check(&self, site: &config::Site) -> SiteHealth {
        let client = if site.allow_insecure {
            &self.insecure
        } else {
            &self.secure
        };
        let mut request = client.get(site.check_url.as_str()).timeout(site.timeout);
        if let Some(auth) = &site.basic_auth {
            request = request.basic_auth(&auth.username, Some(&auth.password));
        }

        let start = Instant::now();
        let response = request.send().await;
        let response_time = start.elapsed();
        let status = match response {
            Ok(response) => {
                let code = response.status().as_u16();
                HealthStatus::Http {
                    code,
                    ok: code == 200 || site.alt_status_codes.contains(&code),
                }
            }
            Err(error) if error.is_timeout() => HealthStatus::TimedOut,
            Err(error) => HealthStatus::Unreachable(error.without_url().to_string()),
        };

        SiteHealth {
            title: site.title.clone(),
            status,
            response_time,
        }
    }
}

#[cfg(test)]
mod tests;
