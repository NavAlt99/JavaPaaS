use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::{Tier, TierConfig};
use crate::error::{DaemonError, Result};

pub struct CgroupManager {
    root: PathBuf,
}

impl Default for CgroupManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CgroupManager {
    pub fn new() -> Self {
        let root = std::env::var("CGROUP_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/sys/fs/cgroup/javapaas"));
        CgroupManager { root }
    }

    #[allow(dead_code)]
    pub fn with_root(root: PathBuf) -> Self {
        CgroupManager { root }
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn init(&self) -> Result<()> {
        let root_controllers_path = Path::new("/sys/fs/cgroup/cgroup.controllers");
        if root_controllers_path.exists() {
            let available = fs::read_to_string(root_controllers_path).unwrap_or_default();
            for ctrl in &["memory", "cpu"] {
                if !available.contains(ctrl) {
                    warn!(
                        "cgroup v2 controller '{ctrl}' is not listed in {:?}. \
                         Resource limits for {ctrl} may not take effect.",
                        root_controllers_path
                    );
                }
            }
        } else if !self.root.starts_with("/tmp") {
            return Err(DaemonError::Cgroup(
                "cgroups v2 not mounted at /sys/fs/cgroup".to_string(),
            ));
        }

        fs::create_dir_all(&self.root).map_err(DaemonError::Io)?;

        let _ = self.write_subtree_control(&self.root, "+cpu +memory +io");

        for tier in &["silver", "gold", "platinum"] {
            let tier_path = self.root.join(tier);
            fs::create_dir_all(&tier_path).map_err(DaemonError::Io)?;
            let _ = self.write_subtree_control(&tier_path, "+cpu +memory +io");
        }

        info!("cgroups v2 hierarchy initialized at {:?}", self.root);
        Ok(())
    }

    fn write_subtree_control(&self, path: &Path, value: &str) -> io::Result<()> {
        let control_file = path.join("cgroup.subtree_control");
        if !control_file.exists() {
            return Ok(());
        }

        let current = fs::read_to_string(&control_file).unwrap_or_default();
        if current.trim() == value {
            return Ok(());
        }

        match fs::write(&control_file, value) {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(
                    "Failed to write '{}' to {:?}: {} (this may require root or parent delegation)",
                    value, control_file, e
                );
                Err(e)
            }
        }
    }

    pub fn validate_identifiers(tier: &str, tenant_id: &str) -> Result<()> {
        if tier.contains('/') || tier.contains('\\') || tier.contains("..") {
            return Err(DaemonError::InvalidInput("invalid tier name".to_string()));
        }
        if tenant_id.is_empty()
            || tenant_id.contains('/')
            || tenant_id.contains('\\')
            || tenant_id.contains("..")
        {
            return Err(DaemonError::InvalidInput(
                "invalid tenant_id: path separators and '..' are forbidden".to_string(),
            ));
        }
        Ok(())
    }

    pub fn create_tenant(&self, tier: &str, tenant_id: &str) -> Result<PathBuf> {
        Self::validate_identifiers(tier, tenant_id)?;
        let parsed_tier = Tier::parse(tier)?;
        let config = TierConfig::for_tier(parsed_tier);

        let tenant_path = self.root.join(tier).join(tenant_id);
        fs::create_dir_all(&tenant_path).map_err(|e| {
            DaemonError::Cgroup(format!(
                "failed to create cgroup dir {}: {e}",
                tenant_path.display()
            ))
        })?;

        let mem_file = tenant_path.join("memory.max");
        if let Err(e) = fs::write(&mem_file, config.cgroup_memory_max.to_string()) {
            warn!("Could not set memory.max on {:?}: {e}", mem_file);
        }

        let swap_file = tenant_path.join("memory.swap.max");
        if let Err(e) = fs::write(&swap_file, config.cgroup_memory_swap_max.to_string()) {
            warn!("Could not set memory.swap.max on {:?}: {e}", swap_file);
        }

        let oom_file = tenant_path.join("memory.oom.group");
        let _ = fs::write(oom_file, "1");

        let cpu_file = tenant_path.join("cpu.max");
        if let Err(e) = fs::write(&cpu_file, config.cpu_max_string()) {
            warn!("Could not set cpu.max on {:?}: {e}", cpu_file);
        }

        info!(
            "Created tenant cgroup: {} with memory.max={} cpu.max={}",
            tenant_path.display(),
            config.cgroup_memory_max,
            config.cpu_max_string()
        );
        Ok(tenant_path)
    }

    pub fn tenant_procs_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id).join("cgroup.procs")
    }

    pub fn tenant_events_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id).join("memory.events")
    }

    #[allow(dead_code)]
    pub fn tenant_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id)
    }

    pub fn read_pids(&self, tier: &str, tenant_id: &str) -> Result<Vec<i32>> {
        let procs_path = self.tenant_procs_path(tier, tenant_id);
        if !procs_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&procs_path).map_err(DaemonError::Io)?;
        let mut pids = Vec::new();
        for line in content.lines() {
            if let Ok(pid) = line.trim().parse::<i32>() {
                pids.push(pid);
            }
        }
        Ok(pids)
    }

    pub fn read_memory_current(&self, tier: &str, tenant_id: &str) -> Result<u64> {
        let path = self.root.join(tier).join(tenant_id).join("memory.current");
        if !path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&path).map_err(DaemonError::Io)?;
        content
            .trim()
            .parse::<u64>()
            .map_err(|e| DaemonError::Cgroup(format!("invalid memory.current: {e}")))
    }

    pub fn read_memory_max(&self, tier: &str, tenant_id: &str) -> Result<u64> {
        let path = self.root.join(tier).join(tenant_id).join("memory.max");
        if !path.exists() {
            let parsed_tier = Tier::parse(tier)?;
            return Ok(TierConfig::for_tier(parsed_tier).cgroup_memory_max);
        }

        let content = fs::read_to_string(&path).map_err(DaemonError::Io)?;
        content
            .trim()
            .parse::<u64>()
            .map_err(|e| DaemonError::Cgroup(format!("invalid memory.max: {e}")))
    }

    pub fn discover_tenants(&self) -> Result<Vec<(String, String, Vec<i32>)>> {
        let mut discovered = Vec::new();
        for tier in &["silver", "gold", "platinum"] {
            let tier_path = self.root.join(tier);
            if let Ok(entries) = fs::read_dir(&tier_path) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let tenant_id = entry.file_name().to_string_lossy().to_string();
                        let pids = self.read_pids(tier, &tenant_id).unwrap_or_default();
                        discovered.push((tier.to_string(), tenant_id, pids));
                    }
                }
            }
        }
        Ok(discovered)
    }

    pub fn resize_tenant(&self, current_tier: &str, tenant_id: &str, new_tier: &str) -> Result<()> {
        Self::validate_identifiers(current_tier, tenant_id)?;
        Self::validate_identifiers(new_tier, tenant_id)?;

        let parsed_new_tier = Tier::parse(new_tier)?;
        let new_config = TierConfig::for_tier(parsed_new_tier);

        let old_path = self.root.join(current_tier).join(tenant_id);
        if !old_path.exists() {
            return Err(DaemonError::NotFound(format!(
                "cgroup for tenant '{tenant_id}' in tier '{current_tier}' not found"
            )));
        }

        if current_tier.eq_ignore_ascii_case(new_tier) {
            let mem_file = old_path.join("memory.max");
            let _ = fs::write(&mem_file, new_config.cgroup_memory_max.to_string());
            let cpu_file = old_path.join("cpu.max");
            let _ = fs::write(&cpu_file, new_config.cpu_max_string());
            info!("Live-updated limits for tenant {tenant_id} in tier {current_tier}");
            return Ok(());
        }

        // Migrate processes to new tier cgroup
        let new_path = self.create_tenant(new_tier, tenant_id)?;
        let pids = self.read_pids(current_tier, tenant_id).unwrap_or_default();
        let new_procs = new_path.join("cgroup.procs");

        for pid in pids {
            let _ = fs::write(&new_procs, pid.to_string());
        }

        // Remove old cgroup
        let _ = self.remove_tenant(current_tier, tenant_id);

        info!("Live-resized and migrated tenant {tenant_id} from {current_tier} to {new_tier}");
        Ok(())
    }

    pub fn remove_tenant(&self, tier: &str, tenant_id: &str) -> Result<()> {
        Self::validate_identifiers(tier, tenant_id)?;
        let path = self.root.join(tier).join(tenant_id);
        if !path.exists() {
            return Ok(());
        }

        // Check if cgroup.kill is supported (Linux 5.14+)
        let kill_path = path.join("cgroup.kill");
        if kill_path.exists() {
            let _ = fs::write(&kill_path, "1");
        }

        // Wait up to 500ms for processes to exit
        for _ in 0..10 {
            let pids = self.read_pids(tier, tenant_id).unwrap_or_default();
            if pids.is_empty() {
                break;
            }
            for pid in pids {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            thread::sleep(Duration::from_millis(50));
        }

        match fs::remove_dir(&path) {
            Ok(()) => {
                info!("Removed tenant cgroup: {:?}", path);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
                // In mock test environments or if non-cgroupfs files remain, remove them
                let _ = fs::remove_dir_all(&path);
                if !path.exists() {
                    Ok(())
                } else {
                    Err(DaemonError::Cgroup(format!(
                        "failed to remove cgroup {}: {e}",
                        path.display()
                    )))
                }
            }
            Err(e) => {
                error!("Failed to remove cgroup directory {:?}: {}", path, e);
                Err(DaemonError::Cgroup(format!(
                    "failed to remove cgroup {}: {e}",
                    path.display()
                )))
            }
        }
    }

    pub fn read_oom_kill_count(&self, tier: &str, tenant_id: &str) -> Result<u64> {
        let events_path = self.tenant_events_path(tier, tenant_id);
        if !events_path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&events_path).map_err(DaemonError::Io)?;
        for line in content.lines() {
            if line.starts_with("oom_kill") {
                if let Some(value) = line.split_whitespace().nth(1) {
                    return value
                        .parse::<u64>()
                        .map_err(|e| DaemonError::Cgroup(format!("invalid oom_kill count: {e}")));
                }
            }
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identifier_validation() {
        assert!(CgroupManager::validate_identifiers("silver", "tenant-1").is_ok());
        assert!(CgroupManager::validate_identifiers("../etc", "tenant-1").is_err());
        assert!(CgroupManager::validate_identifiers("silver", "../etc").is_err());
        assert!(CgroupManager::validate_identifiers("silver", "").is_err());
    }

    #[test]
    fn test_create_and_remove_tenant_mock() {
        let temp_dir = std::env::temp_dir().join(format!("cgroup_test_{}", uuid::Uuid::new_v4()));
        let mgr = CgroupManager::with_root(temp_dir.clone());
        assert!(mgr.init().is_ok());

        let tenant_path = mgr.create_tenant("gold", "tenant-abc").unwrap();
        assert!(tenant_path.exists());
        assert!(tenant_path.join("memory.max").exists());
        assert!(tenant_path.join("memory.swap.max").exists());
        assert!(tenant_path.join("cpu.max").exists());

        let max_mem = fs::read_to_string(tenant_path.join("memory.max")).unwrap();
        assert_eq!(max_mem, "4563402752");

        let cpu_max = fs::read_to_string(tenant_path.join("cpu.max")).unwrap();
        assert_eq!(cpu_max, "200000 100000");

        // Test discovery
        let discovered = mgr.discover_tenants().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0, "gold");
        assert_eq!(discovered[0].1, "tenant-abc");

        assert!(mgr.remove_tenant("gold", "tenant-abc").is_ok());
        assert!(!tenant_path.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
