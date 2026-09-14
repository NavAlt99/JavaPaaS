use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::cgroups::CgroupManager;
use crate::config::{Tier, TierConfig};
use crate::error::{DaemonError, Result};

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

    #[allow(dead_code)]
    pub fn cgroup_manager(&self) -> &Arc<CgroupManager> {
        &self.cgroup_mgr
    }

    pub fn resolve_java_home(&self, version: &str) -> Result<PathBuf> {
        // 1. Check custom /opt/jdk directory
        let java_path = PathBuf::from("/opt/jdk")
            .join(version)
            .join("bin")
            .join("java");
        if java_path.is_file() {
            return Ok(java_path);
        }

        // 2. Check system PATH
        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join("java");
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }

        // 3. Check well-known fallback paths
        let candidates = [
            PathBuf::from("/usr/local/bin/java"),
            PathBuf::from("/usr/bin/java"),
            PathBuf::from("/bin/java"),
        ];
        for c in &candidates {
            if c.is_file() {
                return Ok(c.clone());
            }
        }

        Err(DaemonError::NotFound(format!(
            "Java executable not found for version '{version}' (checked /opt/jdk/{version}/bin/java and system PATH)"
        )))
    }

    pub fn validate_request(&self, req: &ForkRequest) -> Result<()> {
        if req.tenant_id.trim().is_empty() {
            return Err(DaemonError::InvalidInput("tenant_id cannot be empty".to_string()));
        }
        if !req.tenant_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(DaemonError::InvalidInput(
                "tenant_id must contain only alphanumeric characters, '-', or '_'".to_string(),
            ));
        }

        Tier::parse(&req.tier)?;

        if req.java_version.trim().is_empty()
            || req.java_version.contains('/')
            || req.java_version.contains("..")
        {
            return Err(DaemonError::InvalidInput(
                "invalid java_version: path separators and '..' are forbidden".to_string(),
            ));
        }

        if req.jar_path.trim().is_empty() || req.jar_path.contains("..") {
            return Err(DaemonError::InvalidInput(
                "invalid jar_path: empty or path traversal '..' detected".to_string(),
            ));
        }
        if !req.jar_path.ends_with(".jar") {
            return Err(DaemonError::InvalidInput(
                "jar_path must end with .jar extension".to_string(),
            ));
        }

        let jar = Path::new(&req.jar_path);
        if !jar.is_file() {
            return Err(DaemonError::NotFound(format!(
                "JAR file not found: {}",
                req.jar_path
            )));
        }

        // Optional allowlist directory
        if let Ok(allowed_dir) = std::env::var("ALLOWED_APP_DIR") {
            let canonical_jar = fs::canonicalize(jar).map_err(DaemonError::Io)?;
            let canonical_allowed = fs::canonicalize(&allowed_dir).map_err(DaemonError::Io)?;
            if !canonical_jar.starts_with(canonical_allowed) {
                return Err(DaemonError::Unauthorized(format!(
                    "jar_path is outside allowed directory '{allowed_dir}'"
                )));
            }
        }

        Ok(())
    }

    pub fn fork_jvm(&self, req: &ForkRequest) -> Result<u32> {
        self.validate_request(req)?;
        let java_path = self.resolve_java_home(&req.java_version)?;
        let parsed_tier = Tier::parse(&req.tier)?;
        let tier_config = TierConfig::for_tier(parsed_tier);

        // 1. Create tenant cgroup before spawning process
        self.cgroup_mgr.create_tenant(&req.tier, &req.tenant_id)?;

        let procs_path = self
            .cgroup_mgr
            .tenant_procs_path(&req.tier, &req.tenant_id);

        let procs_cstr = match CString::new(procs_path.as_os_str().as_encoded_bytes()) {
            Ok(cs) => cs,
            Err(e) => {
                let _ = self.cgroup_mgr.remove_tenant(&req.tier, &req.tenant_id);
                return Err(DaemonError::InvalidInput(format!("CString error: {e}")));
            }
        };

        // 2. Prepare Command with pre_exec cgroup attachment
        let mut cmd = Command::new(&java_path);
        cmd.args(tier_config.jvm_args());
        cmd.arg("-jar").arg(&req.jar_path);
        cmd.args(&req.extra_args);

        // Pre-exec hook attaches child process to cgroup before exec
        unsafe {
            cmd.pre_exec(move || {
                let fd = libc::open(procs_cstr.as_ptr(), libc::O_WRONLY);
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }

                let mut n = libc::getpid();
                let mut buf = [0u8; 32];
                let mut idx = 31;
                buf[idx] = b'\n';
                if n == 0 {
                    idx -= 1;
                    buf[idx] = b'0';
                } else {
                    while n > 0 && idx > 0 {
                        idx -= 1;
                        buf[idx] = b'0' + (n % 10) as u8;
                        n /= 10;
                    }
                }

                let to_write = 32 - idx;
                let written = libc::write(fd, buf[idx..].as_ptr() as *const libc::c_void, to_write);
                libc::close(fd);

                if written < 0 {
                    return Err(io::Error::last_os_error());
                }

                // Security Sandboxing:
                // 1. Enforce PR_SET_NO_NEW_PRIVS to prevent setuid/capabilities escalation
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }

                // 2. Disable process dumpable flag to protect memory/keys from untrusted ptrace
                let _ = libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);

                Ok(())
            });
        }

        // 3. Spawn child process (propagates exec/pre_exec errors via CLOEXEC pipe)
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                info!(
                    "Spawned JVM (PID {pid}) for tenant {} tier {}",
                    req.tenant_id, req.tier
                );
                Ok(pid)
            }
            Err(e) => {
                error!(
                    "Failed to spawn JVM for tenant {}: {e}. Rolling back cgroup.",
                    req.tenant_id
                );
                let _ = self.cgroup_mgr.remove_tenant(&req.tier, &req.tenant_id);
                Err(DaemonError::Fork(format!("failed to spawn JVM: {e}")))
            }
        }
    }

    pub fn stop_tenant(&self, tier: &str, tenant_id: &str) -> Result<()> {
        CgroupManager::validate_identifiers(tier, tenant_id)?;
        let pids = self.cgroup_mgr.read_pids(tier, tenant_id)?;

        // Send SIGTERM to all running processes in cgroup
        for &pid in &pids {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }

        // Poll for exit up to 3 seconds
        let mut all_exited = false;
        for _ in 0..30 {
            let remaining = self.cgroup_mgr.read_pids(tier, tenant_id).unwrap_or_default();
            if remaining.is_empty() {
                all_exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        // If processes still remain, send SIGKILL
        if !all_exited {
            let remaining = self.cgroup_mgr.read_pids(tier, tenant_id).unwrap_or_default();
            for pid in remaining {
                warn!("Process {pid} ignored SIGTERM, sending SIGKILL");
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            thread::sleep(Duration::from_millis(100));
        }

        // Clean up cgroup directory
        self.cgroup_mgr.remove_tenant(tier, tenant_id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_request_checks() {
        let mgr = Arc::new(CgroupManager::new());
        let forker = JvmForker::new(mgr);

        // Empty tenant ID
        let req = ForkRequest {
            tenant_id: "".to_string(),
            tier: "silver".to_string(),
            java_version: "21".to_string(),
            jar_path: "/tmp/app.jar".to_string(),
            extra_args: vec![],
        };
        assert!(forker.validate_request(&req).is_err());

        // Invalid characters in tenant ID
        let req = ForkRequest {
            tenant_id: "../tenant".to_string(),
            tier: "silver".to_string(),
            java_version: "21".to_string(),
            jar_path: "/tmp/app.jar".to_string(),
            extra_args: vec![],
        };
        assert!(forker.validate_request(&req).is_err());

        // Invalid tier
        let req = ForkRequest {
            tenant_id: "tenant-1".to_string(),
            tier: "unreal".to_string(),
            java_version: "21".to_string(),
            jar_path: "/tmp/app.jar".to_string(),
            extra_args: vec![],
        };
        assert!(forker.validate_request(&req).is_err());

        // Invalid jar extension
        let req = ForkRequest {
            tenant_id: "tenant-1".to_string(),
            tier: "silver".to_string(),
            java_version: "21".to_string(),
            jar_path: "/tmp/app.sh".to_string(),
            extra_args: vec![],
        };
        assert!(forker.validate_request(&req).is_err());
    }

    #[test]
    fn test_java_path_resolution() {
        let mgr = Arc::new(CgroupManager::new());
        let forker = JvmForker::new(mgr);

        // Should find system java if installed or return NotFound cleanly
        let res = forker.resolve_java_home("21");
        if let Ok(p) = res {
            assert!(p.exists());
        }
    }
}
