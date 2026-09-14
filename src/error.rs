use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::fmt;

#[derive(Debug)]
pub enum DaemonError {
    InvalidInput(String),
    Conflict(String),
    NotFound(String),
    Unauthorized(String),
    Cgroup(String),
    Fork(String),
    Io(std::io::Error),
    Nix(nix::Error),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            DaemonError::Conflict(msg) => write!(f, "conflict: {msg}"),
            DaemonError::NotFound(msg) => write!(f, "not found: {msg}"),
            DaemonError::Unauthorized(msg) => write!(f, "unauthorized: {msg}"),
            DaemonError::Cgroup(msg) => write!(f, "cgroup error: {msg}"),
            DaemonError::Fork(msg) => write!(f, "fork error: {msg}"),
            DaemonError::Io(e) => write!(f, "I/O error: {e}"),
            DaemonError::Nix(e) => write!(f, "nix error: {e}"),
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

impl IntoResponse for DaemonError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            DaemonError::InvalidInput(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            DaemonError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            DaemonError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            DaemonError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            DaemonError::Cgroup(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            DaemonError::Fork(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            DaemonError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            DaemonError::Nix(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };

        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, DaemonError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_error_display() {
        let err = DaemonError::InvalidInput("bad tier".to_string());
        assert_eq!(err.to_string(), "invalid input: bad tier");

        let err = DaemonError::NotFound("tenant-1".to_string());
        assert_eq!(err.to_string(), "not found: tenant-1");

        let err = DaemonError::Conflict("already running".to_string());
        assert_eq!(err.to_string(), "conflict: already running");
    }
}
