mod api;
mod cgroups;
mod config;
mod error;
mod jvm_forker;
mod watchdog;

use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let node_id = std::env::var("NODE_ID")
        .unwrap_or_else(|_| get_hostname());

    let listen_addr = std::env::var("LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9100".to_string());

    let controller_url = std::env::var("CONTROLLER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());

    let auth_token = std::env::var("AUTH_TOKEN").ok();

    let cgroup_mgr = Arc::new(cgroups::CgroupManager::new());
    if let Err(e) = cgroup_mgr.init() {
        tracing::error!("Failed to initialize cgroups: {e}");
        std::process::exit(1);
    }
    tracing::info!("cgroups v2 hierarchy initialized");

    let forker = Arc::new(jvm_forker::JvmForker::new(cgroup_mgr.clone()));
    let watchdog = Arc::new(watchdog::HealthWatchdog::new(
        cgroup_mgr.clone(),
        node_id.clone(),
        controller_url,
        auth_token.clone(),
    ));

    let rehydrated = watchdog.rehydrate_tenants().await;
    tracing::info!("Re-hydrated {rehydrated} tenant(s) from cgroup hierarchy on startup");

    let wd = watchdog.clone();
    tokio::spawn(async move {
        wd.start_watchdog().await;
    });

    let app_state = api::AppState {
        forker,
        watchdog,
        node_id,
        auth_token,
    };
    let app = api::create_router(app_state);

    tracing::info!("Starting jvm-lifecycle-mgr on {listen_addr}");
    let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {listen_addr}: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("Server error: {e}");
        std::process::exit(1);
    }
}

fn get_hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "node-unknown".to_string())
    })
}
