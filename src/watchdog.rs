use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

use crate::cgroups::CgroupManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantStatus {
    Running,
    Recovering,
    Stopping,
    Stopped,
    Exited,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CrashEvent {
    pub tenant_id: String,
    pub tier: String,
    pub node_id: String,
    pub reason: String,
    pub exit_code: Option<i32>,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct TenantInfo {
    pub tier: String,
    pub pid: u32,
    pub status: TenantStatus,
    pub generation: u64,
}

pub struct HealthWatchdog {
    cgroup_mgr: Arc<CgroupManager>,
    node_id: String,
    controller_url: String,
    auth_token: Option<String>,
    pub tenants: Arc<Mutex<HashMap<String, TenantInfo>>>,
    oom_counts: Arc<Mutex<HashMap<String, u64>>>,
}

impl HealthWatchdog {
    pub fn new(
        cgroup_mgr: Arc<CgroupManager>,
        node_id: String,
        controller_url: String,
        auth_token: Option<String>,
    ) -> Self {
        HealthWatchdog {
            cgroup_mgr,
            node_id,
            controller_url,
            auth_token,
            tenants: Arc::new(Mutex::new(HashMap::new())),
            oom_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register_tenant(&self, tenant_id: String, tier: String, pid: u32) {
        let mut tenants = self.tenants.lock().await;
        let gen = tenants
            .get(&tenant_id)
            .map(|t| t.generation + 1)
            .unwrap_or(1);

        tenants.insert(
            tenant_id.clone(),
            TenantInfo {
                tier: tier.clone(),
                pid,
                status: TenantStatus::Running,
                generation: gen,
            },
        );

        let initial_oom = self
            .cgroup_mgr
            .read_oom_kill_count(&tier, &tenant_id)
            .unwrap_or(0);
        let mut oom = self.oom_counts.lock().await;
        oom.insert(tenant_id, initial_oom);
    }

    pub async fn set_status(&self, tenant_id: &str, status: TenantStatus) {
        let mut tenants = self.tenants.lock().await;
        if let Some(info) = tenants.get_mut(tenant_id) {
            info.status = status;
        }
    }

    pub async fn update_tenant_tier(&self, tenant_id: &str, new_tier: String) -> bool {
        let mut tenants = self.tenants.lock().await;
        if let Some(info) = tenants.get_mut(tenant_id) {
            info.tier = new_tier;
            true
        } else {
            false
        }
    }

    pub async fn unregister_tenant(&self, tenant_id: &str) {
        let mut tenants = self.tenants.lock().await;
        tenants.remove(tenant_id);
        let mut oom = self.oom_counts.lock().await;
        oom.remove(tenant_id);
    }

    pub async fn get_tenant(&self, tenant_id: &str) -> Option<TenantInfo> {
        let tenants = self.tenants.lock().await;
        tenants.get(tenant_id).cloned()
    }

    pub async fn is_running(&self, tenant_id: &str) -> bool {
        let tenants = self.tenants.lock().await;
        matches!(
            tenants.get(tenant_id).map(|t| t.status),
            Some(TenantStatus::Running)
        )
    }

    pub fn cgroup_manager(&self) -> &Arc<CgroupManager> {
        &self.cgroup_mgr
    }

    pub async fn get_all_tenants(&self) -> HashMap<String, TenantInfo> {
        self.tenants.lock().await.clone()
    }

    pub async fn get_oom_counts(&self) -> HashMap<String, u64> {
        self.oom_counts.lock().await.clone()
    }

    fn is_process_alive(pid: i32) -> bool {
        if pid <= 0 {
            return false;
        }
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(nix::errno::Errno::EPERM) => true,
            _ => false,
        }
    }

    pub async fn rehydrate_tenants(&self) -> usize {
        let discovered = match self.cgroup_mgr.discover_tenants() {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed to scan cgroups for re-hydration: {e}");
                return 0;
            }
        };

        let mut rehydrated = 0;
        let mut tenants = self.tenants.lock().await;
        let mut oom = self.oom_counts.lock().await;

        for (tier, tenant_id, pids) in discovered {
            let alive_pid = pids.into_iter().find(|&pid| Self::is_process_alive(pid));

            if let Some(pid) = alive_pid {
                info!(
                    "Re-hydrated alive tenant {tenant_id} (tier {tier}, PID {pid}) from cgroup"
                );
                tenants.insert(
                    tenant_id.clone(),
                    TenantInfo {
                        tier: tier.clone(),
                        pid: pid as u32,
                        status: TenantStatus::Running,
                        generation: 1,
                    },
                );

                let initial_oom = self
                    .cgroup_mgr
                    .read_oom_kill_count(&tier, &tenant_id)
                    .unwrap_or(0);
                oom.insert(tenant_id, initial_oom);
                rehydrated += 1;
            } else {
                info!("Removing stale/dead cgroup for tenant {tenant_id} in tier {tier}");
                let _ = self.cgroup_mgr.remove_tenant(&tier, &tenant_id);
            }
        }

        rehydrated
    }

    pub async fn start_watchdog(&self) {
        let oom_counts = self.oom_counts.clone();
        let cgroup_mgr = self.cgroup_mgr.clone();
        let tenants_map = self.tenants.clone();
        let node_id = self.node_id.clone();
        let controller_url = self.controller_url.clone();
        let auth_token = self.auth_token.clone();

        // 1. Periodic OOM check
        let oom_task = tokio::spawn(async move {
            let mut tick = interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                let snapshot = {
                    let tenants = tenants_map.lock().await;
                    tenants.clone()
                };

                let mut oom_counts = oom_counts.lock().await;
                for (tenant_id, info) in snapshot {
                    // Only check running tenants
                    if info.status != TenantStatus::Running {
                        continue;
                    }

                    match cgroup_mgr.read_oom_kill_count(&info.tier, &tenant_id) {
                        Ok(current_oom) => {
                            let prev = oom_counts.entry(tenant_id.clone()).or_insert(0);
                            if current_oom > *prev {
                                *prev = current_oom;

                                // Mark as recovering to avoid duplicate report from SIGCHLD
                                {
                                    let mut tenants = tenants_map.lock().await;
                                    if let Some(t) = tenants.get_mut(&tenant_id) {
                                        if t.status != TenantStatus::Running {
                                            continue;
                                        }
                                        t.status = TenantStatus::Recovering;
                                    } else {
                                        continue;
                                    }
                                }

                                let event = CrashEvent {
                                    tenant_id: tenant_id.clone(),
                                    tier: info.tier.clone(),
                                    node_id: node_id.clone(),
                                    reason: "OOM_KILL".to_string(),
                                    exit_code: None,
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                };
                                warn!("OOM kill detected for tenant {}", tenant_id);
                                Self::report_crash(&controller_url, &auth_token, &event).await;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to read OOM count for {}: {}", tenant_id, e);
                        }
                    }
                }
            }
        });

        // 2. SIGCHLD event listener
        let tenants_map2 = self.tenants.clone();
        let node_id2 = self.node_id.clone();
        let controller_url2 = self.controller_url.clone();
        let auth_token2 = self.auth_token.clone();
        let cgroup_mgr2 = self.cgroup_mgr.clone();

        let sigchld_task = tokio::spawn(async move {
            let mut sigchld = match tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::child(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to set up SIGCHLD handler: {}", e);
                    return;
                }
            };

            loop {
                sigchld.recv().await;
                loop {
                    match nix::sys::wait::waitpid(
                        nix::unistd::Pid::from_raw(-1),
                        Some(nix::sys::wait::WaitPidFlag::WNOHANG),
                    ) {
                        Ok(nix::sys::wait::WaitStatus::Exited(pid, code)) => {
                            let mut tenants = tenants_map2.lock().await;
                            let dead_pid = pid.as_raw() as u32;
                            let tenant_id = Self::find_tenant_by_pid(&tenants, dead_pid);
                            let tid = match tenant_id {
                                Some(ref t) => t.clone(),
                                None => continue,
                            };

                            let current_status = tenants.get(&tid).map(|i| i.status);
                            let tier = tenants.get(&tid).map(|i| i.tier.clone()).unwrap_or_default();

                            match current_status {
                                Some(TenantStatus::Stopping) => {
                                    info!("Tenant {tid} stopped cleanly via API (PID {pid})");
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                Some(TenantStatus::Recovering) => {
                                    info!("Tenant {tid} exited while already in recovery (PID {pid})");
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                Some(TenantStatus::Running) => {
                                    warn!("Tenant {tid} exited with code {code} (PID {pid})");
                                    if let Some(t) = tenants.get_mut(&tid) {
                                        if t.pid == dead_pid {
                                            t.status = TenantStatus::Exited;
                                        }
                                    }
                                    drop(tenants);

                                    let event = CrashEvent {
                                        tenant_id: tid.clone(),
                                        tier: tier.clone(),
                                        node_id: node_id2.clone(),
                                        reason: "EXIT".to_string(),
                                        exit_code: Some(code),
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    };
                                    Self::report_crash(&controller_url2, &auth_token2, &event).await;

                                    let mut tenants = tenants_map2.lock().await;
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(nix::sys::wait::WaitStatus::Signaled(pid, sig, _)) => {
                            let mut tenants = tenants_map2.lock().await;
                            let dead_pid = pid.as_raw() as u32;
                            let tenant_id = Self::find_tenant_by_pid(&tenants, dead_pid);
                            let tid = match tenant_id {
                                Some(ref t) => t.clone(),
                                None => continue,
                            };

                            let current_status = tenants.get(&tid).map(|i| i.status);
                            let tier = tenants.get(&tid).map(|i| i.tier.clone()).unwrap_or_default();

                            match current_status {
                                Some(TenantStatus::Stopping) => {
                                    info!("Tenant {tid} killed during intentional stop (PID {pid})");
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                Some(TenantStatus::Recovering) => {
                                    info!("Tenant {tid} killed while already in recovery (PID {pid})");
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                Some(TenantStatus::Running) => {
                                    warn!("Tenant {tid} killed by signal {sig} (PID {pid})");
                                    if let Some(t) = tenants.get_mut(&tid) {
                                        if t.pid == dead_pid {
                                            t.status = TenantStatus::Exited;
                                        }
                                    }
                                    drop(tenants);

                                    let event = CrashEvent {
                                        tenant_id: tid.clone(),
                                        tier: tier.clone(),
                                        node_id: node_id2.clone(),
                                        reason: format!("SIGNAL_{sig}"),
                                        exit_code: None,
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                    };
                                    Self::report_crash(&controller_url2, &auth_token2, &event).await;

                                    let mut tenants = tenants_map2.lock().await;
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                            let _ = cgroup_mgr2.remove_tenant(&tier, &tid);
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(t) = tenants.get(&tid) {
                                        if t.pid == dead_pid {
                                            tenants.remove(&tid);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(nix::sys::wait::WaitStatus::StillAlive) => break,
                        Err(nix::errno::Errno::ECHILD) => break,
                        Err(e) => {
                            error!("waitpid error: {}", e);
                            break;
                        }
                        _ => break,
                    }
                }
            }
        });

        let _ = tokio::join!(oom_task, sigchld_task);
    }

    fn find_tenant_by_pid(
        tenants: &HashMap<String, TenantInfo>,
        pid: u32,
    ) -> Option<String> {
        for (id, info) in tenants.iter() {
            if info.pid == pid {
                return Some(id.clone());
            }
        }
        None
    }

    async fn report_crash(controller_url: &str, auth_token: &Option<String>, event: &CrashEvent) {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/v1/internal/recover",
            controller_url.trim_end_matches('/')
        );
        let mut req = client.post(&url).json(event).timeout(Duration::from_secs(5));
        if let Some(token) = auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        match req.send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    error!(
                        "Controller returned {} for tenant {}",
                        resp.status(),
                        event.tenant_id
                    );
                } else {
                    info!("Reported crash event for tenant {} to controller", event.tenant_id);
                }
            }
            Err(e) => {
                error!("Failed to report crash to controller: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_rehydrate_tenants() {
        let temp_dir = std::env::temp_dir().join(format!("cgroup_rehydrate_{}", uuid::Uuid::new_v4()));
        let mgr = Arc::new(CgroupManager::with_root(temp_dir.clone()));
        assert!(mgr.init().is_ok());

        // Create alive tenant directory with our current PID
        let alive_dir = temp_dir.join("silver").join("alive-tenant");
        fs::create_dir_all(&alive_dir).unwrap();
        let my_pid = std::process::id();
        fs::write(alive_dir.join("cgroup.procs"), format!("{my_pid}\n")).unwrap();

        // Create empty tenant directory (should be cleaned up)
        let empty_dir = temp_dir.join("gold").join("dead-tenant");
        fs::create_dir_all(&empty_dir).unwrap();

        let watchdog = HealthWatchdog::new(
            mgr,
            "node-1".to_string(),
            "http://localhost:8080".to_string(),
            None,
        );

        let count = watchdog.rehydrate_tenants().await;
        assert_eq!(count, 1);

        let tenant = watchdog.get_tenant("alive-tenant").await;
        assert!(tenant.is_some());
        let info = tenant.unwrap();
        assert_eq!(info.tier, "silver");
        assert_eq!(info.pid, my_pid);
        assert_eq!(info.status, TenantStatus::Running);

        // Dead/empty tenant cgroup should have been removed
        assert!(!empty_dir.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
