use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{error, info};

pub struct CgroupManager {
    root: PathBuf,
}

impl CgroupManager {
    pub fn new() -> Self {
        CgroupManager {
            root: PathBuf::from("/sys/fs/cgroup/javapaas"),
        }
    }

    pub fn init(&self) -> io::Result<()> {
        let controllers_path = Path::new("/sys/fs/cgroup/cgroup.controllers");
        if !controllers_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cgroups v2 not mounted at /sys/fs/cgroup/",
            ));
        }

        fs::create_dir_all(&self.root)?;

        self.write_subtree_control("+cpu +memory +io")?;

        for tier in &["silver", "gold", "platinum"] {
            let tier_path = self.root.join(tier);
            fs::create_dir_all(&tier_path)?;
        }

        info!("cgroups v2 hierarchy initialized at {:?}", self.root);
        Ok(())
    }

    fn write_subtree_control(&self, value: &str) -> io::Result<()> {
        let path = self.root.join("cgroup.subtree_control");
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current.trim() == value {
            return Ok(());
        }
        match fs::write(&path, value) {
            Ok(()) => Ok(()),
            Err(e) => {
                error!(
                    "Failed to write '{}' to {:?}: {}",
                    value, path, e
                );
                Err(e)
            }
        }
    }

    pub fn create_tenant(&self, tier: &str, tenant_id: &str) -> io::Result<PathBuf> {
        let tenant_path = self.root.join(tier).join(tenant_id);
        fs::create_dir_all(&tenant_path)?;

        let config = crate::config::TierConfig::for_tier(
            crate::config::Tier::from_str(tier).unwrap_or(crate::config::Tier::Silver),
        );

        fs::write(
            tenant_path.join("memory.max"),
            config.cgroup_memory_max.to_string(),
        )?;

        fs::write(
            tenant_path.join("memory.swap.max"),
            config.cgroup_memory_swap_max.to_string(),
        )?;

        let _ = fs::write(tenant_path.join("memory.oom.group"), "1");

        info!(
            "Created tenant cgroup: {} with memory.max={}",
            tenant_path.display(),
            config.cgroup_memory_max
        );
        Ok(tenant_path)
    }

    pub fn tenant_procs_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id).join("cgroup.procs")
    }

    pub fn tenant_events_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id).join("memory.events")
    }

    pub fn tenant_path(&self, tier: &str, tenant_id: &str) -> PathBuf {
        self.root.join(tier).join(tenant_id)
    }

    pub fn remove_tenant(&self, tier: &str, tenant_id: &str) -> io::Result<()> {
        let path = self.root.join(tier).join(tenant_id);
        if path.exists() {
            let procs_path = path.join("cgroup.procs");
            if procs_path.exists() {
                let _ = fs::write(&procs_path, "");
            }
            match fs::remove_dir(&path) {
                Ok(()) => {
                    info!("Removed tenant cgroup: {:?}", path);
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        }
    }

    pub fn read_oom_kill_count(&self, tier: &str, tenant_id: &str) -> io::Result<u64> {
        let events_path = self.tenant_events_path(tier, tenant_id);
        let content = fs::read_to_string(&events_path)?;
        for line in content.lines() {
            if line.starts_with("oom_kill") {
                if let Some(value) = line.split_whitespace().nth(1) {
                    return value
                        .parse::<u64>()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
                }
            }
        }
        Ok(0)
    }
}
