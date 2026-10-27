use osm_surface::api;
use osm_surface::config::Config;
use osm_surface::trace::Service;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if std::env::var("LOG_FORMAT").is_ok_and(|v| v == "json") {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let config = Config::from_env();
    tracing::info!(?config, "starting osm-surface");
    let addr = config.listen_addr.clone();
    let service = Arc::new(Service::new(config)?);
    {
        let service = service.clone();
        tokio::spawn(async move { service.fetch_tiles_info().await });
    }
    spawn_maintenance(service.clone());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, api::router(service))
        .with_graceful_shutdown(async {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

/// Background chores: the disk cache sweep (at startup, then every 6 hours) and the reload
/// of the OSM index when its file changes (checked every 5 minutes).
fn spawn_maintenance(service: Arc<Service>) {
    if let Some(root) = service.config.cache_dir.clone() {
        let max_age = service.config.cache_ttl * 2;
        let max_bytes = service.config.cache_max_bytes;
        tokio::spawn(async move {
            loop {
                let root = root.clone();
                let r = tokio::task::spawn_blocking(move || {
                    osm_surface::tiles::sweep_cache(&root, max_age, max_bytes)
                })
                .await;
                if let Ok((removed, bytes)) = r {
                    tracing::info!(removed, mb = bytes / 1_000_000, "tile cache swept");
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
    }
    if service.config.osm_index.is_some() {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                let s = service.clone();
                let _ = tokio::task::spawn_blocking(move || s.osm.reload_if_changed()).await;
            }
        });
    }
}
