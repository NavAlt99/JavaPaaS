use std::fmt;

#[derive(Debug)]
pub enum DaemonError {
    Cgroup(String),
    Fork(String),
    Io(std::io::Error),
    Nix(nix::Error),
    InvalidConfig(String),
    NotFound(String),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::Cgroup(msg) => write!(f, "cgroup error: {}", msg),
            DaemonError::Fork(msg) => write!(f, "fork error: {}", msg),
            DaemonError::Io(e) => write!(f, "I/O error: {}", e),
            DaemonError::Nix(e) => write!(f, "nix error: {}", e),
            DaemonError::InvalidConfig(msg) => write!(f, "invalid config: {}", msg),
            DaemonError::NotFound(msg) => write!(f, "not found: {}", msg),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DaemonError::Io(e) => Some(e),
            DaemonError::Nix(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(e: std::io::Error) -> Self {
        DaemonError::Io(e)
    }
}

impl From<nix::Error> for DaemonError {
    fn from(e: nix::Error) -> Self {
        DaemonError::Nix(e)
    }
}
