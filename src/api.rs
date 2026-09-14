use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::error::{DaemonError, Result};
use crate::jvm_forker::{ForkRequest, JvmForker};
use crate::watchdog::{HealthWatchdog, TenantStatus};

#[derive(Clone)]
pub struct AppState {
    pub forker: Arc<JvmForker>,
    pub watchdog: Arc<HealthWatchdog>,
    pub node_id: String,
    pub auth_token: Option<String>,
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

#[derive(Deserialize)]
pub struct ResizePayload {
    pub new_tier: String,
}

#[derive(Serialize)]
pub struct ResizeResponse {
    pub tenant_id: String,
    pub tier: String,
    pub status: String,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/fork", post(handle_fork))
        .route("/stop/{tenant_id}", post(handle_stop))
        .route("/resize/{tenant_id}", post(handle_resize).put(handle_resize))
        .route("/status/{tenant_id}", get(handle_status))
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .with_state(state)
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let expected_token = match &state.auth_token {
        Some(token) if !token.is_empty() => token,
        _ => return Ok(()),
    };

    if let Some(auth_val) = headers.get("Authorization") {
        if let Ok(auth_str) = auth_val.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                if token.trim() == expected_token {
                    return Ok(());
                }
            }
        }
    }

    if let Some(token_val) = headers.get("X-JavaPaaS-Token") {
        if let Ok(token_str) = token_val.to_str() {
            if token_str.trim() == expected_token {
                return Ok(());
            }
        }
    }

    Err(DaemonError::Unauthorized(
        "unauthorized: missing or invalid authorization token".to_string(),
    ))
}

async fn handle_fork(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ForkPayload>,
) -> Result<impl IntoResponse> {
    check_auth(&state, &headers)?;

    if state.watchdog.is_running(&payload.tenant_id).await {
        return Err(DaemonError::Conflict(format!(
            "tenant '{}' is already running",
            payload.tenant_id
        )));
    }

    let req = ForkRequest {
        tenant_id: payload.tenant_id.clone(),
        tier: payload.tier.clone(),
        java_version: payload.java_version.clone(),
        jar_path: payload.jar_path.clone(),
        extra_args: payload.extra_args.clone(),
    };

    match state.forker.fork_jvm(&req) {
        Ok(pid) => {
            info!(
                "Forked JVM for tenant {} with PID {}",
                payload.tenant_id, pid
            );
            state
                .watchdog
                .register_tenant(payload.tenant_id.clone(), payload.tier.clone(), pid)
                .await;
            Ok((
                StatusCode::OK,
                Json(ForkResponse {
                    tenant_id: payload.tenant_id,
                    pid,
                    status: "running".to_string(),
                }),
            ))
        }
        Err(e) => {
            error!(
                "Failed to fork JVM for tenant {}: {}",
                payload.tenant_id, e
            );
            Err(e)
        }
    }
}

async fn handle_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant_id): Path<String>,
) -> Result<impl IntoResponse> {
    check_auth(&state, &headers)?;

    let tenant_info = state
        .watchdog
        .get_tenant(&tenant_id)
        .await
        .ok_or_else(|| DaemonError::NotFound(format!("tenant '{tenant_id}' not found")))?;

    // Suppress crash alerts during deliberate manual stop
    state
        .watchdog
        .set_status(&tenant_id, TenantStatus::Stopping)
        .await;

    match state.forker.stop_tenant(&tenant_info.tier, &tenant_id) {
        Ok(()) => {
            state
                .watchdog
                .set_status(&tenant_id, TenantStatus::Stopped)
                .await;
            state.watchdog.unregister_tenant(&tenant_id).await;
            info!("Stopped tenant {}", tenant_id);
            Ok((
                StatusCode::OK,
                Json(ForkResponse {
                    tenant_id,
                    pid: 0,
                    status: "stopped".to_string(),
                }),
            ))
        }
        Err(e) => {
            error!("Failed to stop tenant {}: {}", tenant_id, e);
            Err(e)
        }
    }
}

async fn handle_resize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant_id): Path<String>,
    Json(payload): Json<ResizePayload>,
) -> Result<impl IntoResponse> {
    check_auth(&state, &headers)?;

    let current_info = state
        .watchdog
        .get_tenant(&tenant_id)
        .await
        .ok_or_else(|| DaemonError::NotFound(format!("tenant '{tenant_id}' not found")))?;

    crate::config::Tier::parse(&payload.new_tier)?;

    state
        .forker
        .cgroup_manager()
        .resize_tenant(&current_info.tier, &tenant_id, &payload.new_tier)?;

    state
        .watchdog
        .update_tenant_tier(&tenant_id, payload.new_tier.clone())
        .await;

    info!(
        "Successfully resized tenant {} from {} to {}",
        tenant_id, current_info.tier, payload.new_tier
    );

    Ok((
        StatusCode::OK,
        Json(ResizeResponse {
            tenant_id,
            tier: payload.new_tier,
            status: "resized".to_string(),
        }),
    ))
}

async fn handle_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant_id): Path<String>,
) -> Result<impl IntoResponse> {
    check_auth(&state, &headers)?;

    match state.watchdog.get_tenant(&tenant_id).await {
        Some(info) => Ok((
            StatusCode::OK,
            Json(StatusResponse {
                tenant_id,
                pid: Some(info.pid),
                tier: Some(info.tier),
                status: match info.status {
                    TenantStatus::Running => "running".to_string(),
                    TenantStatus::Recovering => "recovering".to_string(),
                    TenantStatus::Stopping => "stopping".to_string(),
                    TenantStatus::Stopped => "stopped".to_string(),
                    TenantStatus::Exited => "exited".to_string(),
                },
            }),
        )),
        None => Err(DaemonError::NotFound(format!(
            "tenant '{tenant_id}' not found"
        ))),
    }
}

async fn handle_health(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "node_id": state.node_id,
    }))
}

async fn handle_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let tenants = state.watchdog.get_all_tenants().await;
    let oom_counts = state.watchdog.get_oom_counts().await;
    let cgroup_mgr = state.watchdog.cgroup_manager();

    let mut out = String::new();

    out.push_str("# HELP javapaas_active_tenants Number of currently active tenants\n");
    out.push_str("# TYPE javapaas_active_tenants gauge\n");
    let active_count = tenants
        .values()
        .filter(|t| t.status == TenantStatus::Running)
        .count();
    out.push_str(&format!("javapaas_active_tenants {active_count}\n\n"));

    out.push_str("# HELP javapaas_tenant_memory_current_bytes Current memory usage in bytes per tenant\n");
    out.push_str("# TYPE javapaas_tenant_memory_current_bytes gauge\n");
    for (tenant_id, info) in &tenants {
        let mem = cgroup_mgr
            .read_memory_current(&info.tier, tenant_id)
            .unwrap_or(0);
        out.push_str(&format!(
            "javapaas_tenant_memory_current_bytes{{tenant_id=\"{tenant_id}\",tier=\"{}\"}} {mem}\n",
            info.tier
        ));
    }
    out.push('\n');

    out.push_str("# HELP javapaas_tenant_memory_max_bytes Configured memory limit in bytes per tenant\n");
    out.push_str("# TYPE javapaas_tenant_memory_max_bytes gauge\n");
    for (tenant_id, info) in &tenants {
        let max_mem = cgroup_mgr
            .read_memory_max(&info.tier, tenant_id)
            .unwrap_or(0);
        out.push_str(&format!(
            "javapaas_tenant_memory_max_bytes{{tenant_id=\"{tenant_id}\",tier=\"{}\"}} {max_mem}\n",
            info.tier
        ));
    }
    out.push('\n');

    out.push_str("# HELP javapaas_tenant_oom_kills_total Cumulative OOM kill events per tenant\n");
    out.push_str("# TYPE javapaas_tenant_oom_kills_total counter\n");
    for (tenant_id, info) in &tenants {
        let ooms = oom_counts.get(tenant_id).copied().unwrap_or(0);
        out.push_str(&format!(
            "javapaas_tenant_oom_kills_total{{tenant_id=\"{tenant_id}\",tier=\"{}\"}} {ooms}\n",
            info.tier
        ));
    }
    out.push('\n');

    out.push_str("# HELP javapaas_tenant_status Current tenant status (1 if matching state)\n");
    out.push_str("# TYPE javapaas_tenant_status gauge\n");
    for (tenant_id, info) in &tenants {
        let status_str = match info.status {
            TenantStatus::Running => "running",
            TenantStatus::Recovering => "recovering",
            TenantStatus::Stopping => "stopping",
            TenantStatus::Stopped => "stopped",
            TenantStatus::Exited => "exited",
        };
        out.push_str(&format!(
            "javapaas_tenant_status{{tenant_id=\"{tenant_id}\",tier=\"{}\",status=\"{status_str}\"}} 1\n",
            info.tier
        ));
    }

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgroups::CgroupManager;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_health_endpoint() {
        let mgr = Arc::new(CgroupManager::new());
        let forker = Arc::new(JvmForker::new(mgr.clone()));
        let watchdog = Arc::new(HealthWatchdog::new(
            mgr,
            "test-node".to_string(),
            "http://localhost:8080".to_string(),
            None,
        ));

        let app = create_router(AppState {
            forker,
            watchdog,
            node_id: "test-node".to_string(),
            auth_token: None,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_rejection() {
        let mgr = Arc::new(CgroupManager::new());
        let forker = Arc::new(JvmForker::new(mgr.clone()));
        let watchdog = Arc::new(HealthWatchdog::new(
            mgr,
            "test-node".to_string(),
            "http://localhost:8080".to_string(),
            Some("secret-key".to_string()),
        ));

        let app = create_router(AppState {
            forker,
            watchdog,
            node_id: "test-node".to_string(),
            auth_token: Some("secret-key".to_string()),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_metrics_endpoint() {
        let mgr = Arc::new(CgroupManager::new());
        let forker = Arc::new(JvmForker::new(mgr.clone()));
        let watchdog = Arc::new(HealthWatchdog::new(
            mgr,
            "test-node".to_string(),
            "http://localhost:8080".to_string(),
            None,
        ));

        let app = create_router(AppState {
            forker,
            watchdog,
            node_id: "test-node".to_string(),
            auth_token: None,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_resize_endpoint() {
        let temp_dir = std::env::temp_dir().join(format!("api_resize_{}", uuid::Uuid::new_v4()));
        let mgr = Arc::new(CgroupManager::with_root(temp_dir.clone()));
        assert!(mgr.init().is_ok());
        let _ = mgr.create_tenant("silver", "resize-tenant").unwrap();

        let forker = Arc::new(JvmForker::new(mgr.clone()));
        let watchdog = Arc::new(HealthWatchdog::new(
            mgr.clone(),
            "test-node".to_string(),
            "http://localhost:8080".to_string(),
            None,
        ));
        watchdog
            .register_tenant("resize-tenant".to_string(), "silver".to_string(), 1234)
            .await;

        let app = create_router(AppState {
            forker,
            watchdog: watchdog.clone(),
            node_id: "test-node".to_string(),
            auth_token: None,
        });

        let payload = serde_json::json!({
            "new_tier": "gold"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resize/resize-tenant")
                    .header("Content-Type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify tier updated in watchdog
        let t = watchdog.get_tenant("resize-tenant").await.unwrap();
        assert_eq!(t.tier, "gold");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
