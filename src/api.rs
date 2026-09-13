use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};
use crate::jvm_forker::{ForkRequest, JvmForker};
use crate::watchdog::HealthWatchdog;

#[derive(Clone)]
pub struct AppState {
    pub forker: Arc<JvmForker>,
    pub watchdog: Arc<HealthWatchdog>,
    pub node_id: String,
}

#[derive(Deserialize)]
pub struct ForkPayload {
    pub tenant_id: String,
    pub tier: String,
    pub java_version: String,
    pub jar_path: String,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Serialize)]
pub struct ForkResponse {
    pub tenant_id: String,
    pub pid: u32,
    pub status: String,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub tenant_id: String,
    pub pid: Option<u32>,
    pub tier: Option<String>,
    pub status: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/fork", post(handle_fork))
        .route("/stop/{tenant_id}", post(handle_stop))
        .route("/status/{tenant_id}", get(handle_status))
        .route("/health", get(handle_health))
        .with_state(state)
}

async fn handle_fork(
    State(state): State<AppState>,
    Json(payload): Json<ForkPayload>,
) -> impl IntoResponse {
    let req = ForkRequest {
        tenant_id: payload.tenant_id.clone(),
        tier: payload.tier.clone(),
        java_version: payload.java_version.clone(),
        jar_path: payload.jar_path.clone(),
        extra_args: payload.extra_args.clone(),
    };

    match state.forker.fork_jvm(&req) {
        Ok(pid) => {
            info!("Forked JVM for tenant {} with PID {}", payload.tenant_id, pid);
            state
                .watchdog
                .register_tenant(payload.tenant_id.clone(), payload.tier.clone(), pid)
                .await;
            Ok(Json(ForkResponse {
                tenant_id: payload.tenant_id,
                pid,
                status: "running".to_string(),
            }))
        }
        Err(e) => {
            error!("Failed to fork JVM for tenant {}: {}", payload.tenant_id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("fork failed: {e}"),
                }),
            ))
        }
    }
}

async fn handle_stop(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> impl IntoResponse {
    let tenants = state.watchdog.tenants.lock().await;
    let tier = tenants.get(&tenant_id).map(|t| t.tier.clone());
    drop(tenants);

    let tier = match tier {
        Some(t) => t,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("tenant {tenant_id} not found"),
                }),
            ));
        }
    };

    match state.forker.stop_tenant(&tier, &tenant_id) {
        Ok(()) => {
            state.watchdog.unregister_tenant(&tenant_id).await;
            info!("Stopped tenant {}", tenant_id);
            Ok(Json(ForkResponse {
                tenant_id,
                pid: 0,
                status: "stopped".to_string(),
            }))
        }
        Err(e) => {
            error!("Failed to stop tenant {}: {}", tenant_id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("stop failed: {e}"),
                }),
            ))
        }
    }
}

async fn handle_status(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Json<StatusResponse> {
    let tenants = state.watchdog.tenants.lock().await;
    match tenants.get(&tenant_id) {
        Some(info) => Json(StatusResponse {
            tenant_id,
            pid: Some(info.pid),
            tier: Some(info.tier.clone()),
            status: "running".to_string(),
        }),
        None => Json(StatusResponse {
            tenant_id,
            pid: None,
            tier: None,
            status: "not_found".to_string(),
        }),
    }
}

async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
    }))
}
