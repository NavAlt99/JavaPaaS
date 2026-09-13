use std::ffi::CString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use nix::unistd::{fork, ForkResult};
use tracing::info;
use crate::cgroups::CgroupManager;
use crate::config::{Tier, TierConfig};

pub struct JvmForker {
    cgroup_mgr: Arc<CgroupManager>,
}

#[derive(Debug, Clone)]
pub struct ForkRequest {
    pub tenant_id: String,
    pub tier: String,
    pub java_version: String,
    pub jar_path: String,
    pub extra_args: Vec<String>,
}

impl JvmForker {
    pub fn new(cgroup_mgr: Arc<CgroupManager>) -> Self {
        JvmForker { cgroup_mgr }
    }

    pub fn resolve_java_home(&self, version: &str) -> io::Result<PathBuf> {
        let java_path = PathBuf::from("/opt/jdk")
            .join(version)
            .join("bin")
            .join("java");
        if java_path.exists() {
            return Ok(java_path);
        }

        let candidates = vec![
            PathBuf::from("/usr/local/bin/java"),
            PathBuf::from("/usr/bin/java"),
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.clone());
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "JDK not found at /opt/jdk/{version}/bin/java \
                 and no system java found in PATH candidates"
            ),
        ))
    }

    pub fn fork_jvm(&self, req: &ForkRequest) -> io::Result<u32> {
        let java_path = self.resolve_java_home(&req.java_version)?;
        let procs_path = self
            .cgroup_mgr
            .tenant_procs_path(&req.tier, &req.tenant_id);

        let tier_config = TierConfig::for_tier(
            Tier::from_str(&req.tier).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown tier: {}", req.tier),
                )
            })?,
        );

        let mut jvm_args = vec![java_path.to_string_lossy().to_string()];
        jvm_args.extend(tier_config.jvm_args());
        jvm_args.push("-jar".to_string());
        jvm_args.push(req.jar_path.clone());
        jvm_args.extend(req.extra_args.clone());

        let java_cstr = CString::new(java_path.as_os_str().as_encoded_bytes()).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("CString error: {e}"))
        })?;

        let args_cstrings: Result<Vec<CString>, _> = jvm_args
            .iter()
            .map(|a| {
                CString::new(a.as_bytes()).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("CString error: {e}"))
                })
            })
            .collect();
        let args_cstrings = args_cstrings?;
        let args_refs: Vec<&CString> = args_cstrings.iter().collect();

        let child_pid: i32 = match unsafe { fork()? } {
            ForkResult::Parent { child } => {
                info!(
                    "Forked JVM (PID {}) for tenant {} tier {}",
                    child, req.tenant_id, req.tier
                );
                child.as_raw()
            }
            ForkResult::Child => {
                let my_pid = std::process::id();
                if let Err(e) = fs::write(&procs_path, my_pid.to_string()) {
                    let _ = e;
                    std::process::exit(1);
                }

                let _ = nix::unistd::execvp(&java_cstr, &args_refs);
                std::process::exit(1)
            }
        };

        Ok(child_pid as u32)
    }

    pub fn stop_tenant(&self, tier: &str, tenant_id: &str) -> io::Result<()> {
        let procs_path = self.cgroup_mgr.tenant_procs_path(tier, tenant_id);
        let content = fs::read_to_string(&procs_path)?;
        for line in content.lines() {
            let pid: i32 = line.trim().parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid PID: {e}"))
            })?;
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        self.cgroup_mgr.remove_tenant(tier, tenant_id)?;
        Ok(())
    }
}
