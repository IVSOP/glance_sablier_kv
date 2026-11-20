use std::{error::Error, future::IntoFuture, io, net::SocketAddr, time::Duration};

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Check service health on demand and store the latest Sablier container state"
)]
struct Config {
    /// Address on which to serve HTTP requests.
    #[arg(long, env = "LISTEN_ADDR", default_value = "127.0.0.1:3000")]
    listen_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(error) if error.not_found() => {}
        Err(error) => return Err(error.into()),
    }

    let config = Config::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let app = sablier_mini_kv::app()?;
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    tracing::info!(address = %listener.local_addr()?, "listening");

    let shutdown = CancellationToken::new();
    let signal_task = tokio::spawn(cancel_on_signal(shutdown.clone()));
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    tokio::pin!(server);

    let result = tokio::select! {
        result = &mut server => result,
        () = shutdown.cancelled() => {
            tracing::info!("shutting down");
            match tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("shutdown grace period expired");
                    Ok(())
                }
            }
        }
    };

    signal_task.abort();
    result?;
    Ok(())
}

async fn cancel_on_signal(shutdown: CancellationToken) {
    if let Err(error) = shutdown_signal().await {
        tracing::error!(%error, "failed to listen for a shutdown signal");
    }
    shutdown.cancel();
}

async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
