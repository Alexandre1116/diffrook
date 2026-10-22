use axum::Router;
use std::net::SocketAddr;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data_dir =
        env_value("OVERSIGHT_DATA_DIR", "DIFFROOK_DATA_DIR").unwrap_or_else(|| "./data".into());
    if oversight::updates::exec_active(std::path::Path::new(&data_dir))? {
        unreachable!();
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let state = oversight::core::AppState::new(data_dir).await?;
    oversight::updates::start(state.clone());
    oversight::routes::start_worker(state.clone());
    oversight::scheduler::start(state.clone());
    let web_dir =
        env_value("OVERSIGHT_WEB_DIR", "DIFFROOK_WEB_DIR").unwrap_or_else(|| "./web/dist".into());
    let index = std::path::Path::new(&web_dir).join("index.html");
    let restart_state = state.clone();
    let security_state = state.clone();
    let app: Router = oversight::routes::router(state)
        .fallback_service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(index)))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            2 * 1024 * 1024,
        ))
        .layer(axum::middleware::from_fn_with_state(
            security_state,
            oversight::security::protect_static,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                // Authorization codes and state are query parameters: never log them.
                tracing::info_span!("http", method = %request.method(), path = request.uri().path())
            },
        ));
    let host = env_value("OVERSIGHT_HOST", "DIFFROOK_HOST").unwrap_or_else(|| "127.0.0.1".into());
    let port = env_value("OVERSIGHT_PORT", "DIFFROOK_PORT")
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    oversight::updates::confirm_active(std::path::Path::new(
        &env_value("OVERSIGHT_DATA_DIR", "DIFFROOK_DATA_DIR").unwrap_or_else(|| "./data".into()),
    ))?;
    tracing::info!("Oversight listening on {addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move { tokio::select! { _ = shutdown_signal() => {}, _ = oversight::updates::restart_signal(&restart_state) => {} } })
        .await?;
    oversight::updates::exec_active(std::path::Path::new(
        &env_value("OVERSIGHT_DATA_DIR", "DIFFROOK_DATA_DIR").unwrap_or_else(|| "./data".into()),
    ))?;
    Ok(())
}

fn env_value(primary: &str, legacy: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .or_else(|| std::env::var(legacy).ok())
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
