use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};
use tracing::{error, warn};
use crate::cgroups::CgroupManager;

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
}

pub struct HealthWatchdog {
    cgroup_mgr: Arc<CgroupManager>,
    node_id: String,
    controller_url: String,
    pub tenants: Arc<Mutex<HashMap<String, TenantInfo>>>,
    oom_counts: Arc<Mutex<HashMap<String, u64>>>,
}

impl HealthWatchdog {
    pub fn new(
        cgroup_mgr: Arc<CgroupManager>,
        node_id: String,
        controller_url: String,
    ) -> Self {
        HealthWatchdog {
            cgroup_mgr,
            node_id,
            controller_url,
            tenants: Arc::new(Mutex::new(HashMap::new())),
            oom_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register_tenant(&self, tenant_id: String, tier: String, pid: u32) {
        let mut tenants = self.tenants.lock().await;
        tenants.insert(tenant_id.clone(), TenantInfo { tier, pid });
        let mut oom = self.oom_counts.lock().await;
        oom.insert(tenant_id, 0);
    }

    pub async fn unregister_tenant(&self, tenant_id: &str) {
        let mut tenants = self.tenants.lock().await;
        tenants.remove(tenant_id);
        let mut oom = self.oom_counts.lock().await;
        oom.remove(tenant_id);
    }

    pub async fn start_watchdog(&self) {
        let oom_counts = self.oom_counts.clone();
        let cgroup_mgr = self.cgroup_mgr.clone();
        let tenants_map = self.tenants.clone();
        let node_id = self.node_id.clone();
        let controller_url = self.controller_url.clone();

        let oom_task = tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let snapshot = {
                    let tenants = tenants_map.lock().await;
                    tenants.clone()
                };
                let mut oom_counts = oom_counts.lock().await;
                for (tenant_id, info) in &snapshot {
                    match cgroup_mgr.read_oom_kill_count(&info.tier, tenant_id) {
                        Ok(current_oom) => {
                            let prev = oom_counts.entry(tenant_id.clone()).or_insert(0);
                            if current_oom > *prev {
                                *prev = current_oom;
                                let event = CrashEvent {
                                    tenant_id: tenant_id.clone(),
                                    tier: info.tier.clone(),
                                    node_id: node_id.clone(),
                                    reason: "OOM_KILL".to_string(),
                                    exit_code: None,
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                };
                                warn!("OOM kill detected for tenant {}", tenant_id);
                                Self::report_crash(&controller_url, &event).await;
                            }
                        }
                        Err(e) => {
                            if e.kind() != std::io::ErrorKind::NotFound {
                                error!("Failed to read OOM count for {}: {}", tenant_id, e);
                            }
                        }
                    }
                }
            }
        });

        let tenants_map2 = self.tenants.clone();
        let node_id2 = self.node_id.clone();
        let controller_url2 = self.controller_url.clone();
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
                            let tenants = tenants_map2.lock().await;
                            let tenant_id = Self::find_tenant_by_pid(&tenants, pid.as_raw() as u32);
                            let info = tenant_id.as_ref().and_then(|id| tenants.get(id));
                            let tier = info.map(|i| i.tier.clone()).unwrap_or_default();
                            let tid = match tenant_id {
                                Some(ref t) => t.clone(),
                                None => continue,
                            };
                            drop(tenants);

                            warn!("Tenant {} exited with code {} (PID {})", tid, code, pid);
                            let event = CrashEvent {
                                tenant_id: tid.clone(),
                                tier,
                                node_id: node_id2.clone(),
                                reason: "EXIT".to_string(),
                                exit_code: Some(code),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            };
                            Self::report_crash(&controller_url2, &event).await;

                            let mut tenants = tenants_map2.lock().await;
                            tenants.remove(&tid);
                            let _ = cgroup_mgr2.remove_tenant(&event.tier, &tid);
                        }
                        Ok(nix::sys::wait::WaitStatus::Signaled(pid, sig, _)) => {
                            let tenants = tenants_map2.lock().await;
                            let tenant_id = Self::find_tenant_by_pid(&tenants, pid.as_raw() as u32);
                            let info = tenant_id.as_ref().and_then(|id| tenants.get(id));
                            let tier = info.map(|i| i.tier.clone()).unwrap_or_default();
                            let tid = match tenant_id {
                                Some(ref t) => t.clone(),
                                None => continue,
                            };
                            drop(tenants);

                            warn!(
                                "Tenant {} killed by signal {} (PID {})",
                                tid, sig, pid
                            );
                            let event = CrashEvent {
                                tenant_id: tid.clone(),
                                tier,
                                node_id: node_id2.clone(),
                                reason: format!("SIGNAL_{sig}"),
                                exit_code: None,
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            };
                            Self::report_crash(&controller_url2, &event).await;

                            let mut tenants = tenants_map2.lock().await;
                            tenants.remove(&tid);
                            let _ = cgroup_mgr2.remove_tenant(&event.tier, &tid);
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

    async fn report_crash(controller_url: &str, event: &CrashEvent) {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/v1/internal/recover",
            controller_url.trim_end_matches('/')
        );
        match client
            .post(&url)
            .json(event)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    error!(
                        "Controller returned {} for tenant {}",
                        resp.status(),
                        event.tenant_id
                    );
                }
            }
            Err(e) => {
                error!("Failed to report crash to controller: {}", e);
            }
        }
    }
}
