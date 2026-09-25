use axum::Router;
use std::net::SocketAddr;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let data_dir = std::env::var("DIFFROOK_DATA_DIR").unwrap_or_else(|_| "./data".into());
    let state = diffrook::core::AppState::new(data_dir).await?;
    diffrook::routes::start_worker(state.clone());
    diffrook::scheduler::start(state.clone());
    let web_dir = std::env::var("DIFFROOK_WEB_DIR").unwrap_or_else(|_| "./web/dist".into());
    let index = std::path::Path::new(&web_dir).join("index.html");
    let app: Router = diffrook::routes::router(state)
        .fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(index)))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            2 * 1024 * 1024,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http());
    let host = std::env::var("DIFFROOK_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("DIFFROOK_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Diffrook listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
