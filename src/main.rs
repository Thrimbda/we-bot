use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;
use we_bot::{
    config::Config, http::build_router, service::NotificationService, wechat::IlinkProvider,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "we_bot=info".into()))
        .with_target(false)
        .init();

    let config = Config::from_env()?;
    let cancellation_token = CancellationToken::new();
    let provider = IlinkProvider::new(config.state_path, cancellation_token.child_token())?;
    provider.start().await;
    let service = NotificationService::new(
        Arc::new(provider.clone()),
        config.dedupe_ttl,
        config.rate_limit_per_minute,
    );
    let app = build_router(
        service,
        provider,
        config.api_token,
        config.allowed_hosts,
        cancellation_token.child_token(),
        config.console_auth,
    );
    let listener = TcpListener::bind(config.bind_addr).await?;
    info!(address = %config.bind_addr, "we-bot listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancellation_token))
        .await?;
    Ok(())
}

async fn shutdown_signal(cancellation_token: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install Ctrl-C handler");

    cancellation_token.cancel();
}
