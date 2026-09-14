package main

import (
	"fmt"
	"strings"
)

type TenantCrashEvent struct {
	TenantID  string `json:"tenant_id"`
	Tier      string `json:"tier"`
	NodeID    string `json:"node_id"`
	Reason    string `json:"reason"`
	ExitCode  *int   `json:"exit_code,omitempty"`
	Timestamp string `json:"timestamp"`
}

type TenantSpec struct {
	NodeID          string   `json:"node_id"`
	JavaVersion     string   `json:"java_version"`
	Tier            string   `json:"tier"`
	JarPath         string   `json:"jar_path"`
	ExtraArgs       []string `json:"extra_args"`
	HealthCheckPath string   `json:"health_check_path,omitempty"`
	HealthCheckPort int      `json:"health_check_port,omitempty"`
	Database        string   `json:"database,omitempty"`
}

func (s *TenantSpec) Validate() error {
	if strings.TrimSpace(s.NodeID) == "" {
		return fmt.Errorf("node_id cannot be empty")
	}
	if strings.TrimSpace(s.JavaVersion) == "" {
		return fmt.Errorf("java_version cannot be empty")
	}
	tier := strings.ToLower(strings.TrimSpace(s.Tier))
	if tier != "silver" && tier != "gold" && tier != "platinum" {
		return fmt.Errorf("invalid tier '%s': must be silver, gold, or platinum", s.Tier)
	}
	if strings.TrimSpace(s.JarPath) == "" || strings.Contains(s.JarPath, "..") || !strings.HasSuffix(s.JarPath, ".jar") {
		return fmt.Errorf("jar_path must be non-empty, end with .jar, and not contain '..'")
	}
	if s.HealthCheckPort < 0 || s.HealthCheckPort > 65535 {
		return fmt.Errorf("invalid health_check_port %d: must be between 1 and 65535", s.HealthCheckPort)
	}
	return nil
}

type TenantRegistrationRequest struct {
	TenantID        string   `json:"tenant_id"`
	NodeID          string   `json:"node_id"`
	JavaVersion     string   `json:"java_version"`
	Tier            string   `json:"tier"`
	JarPath         string   `json:"jar_path"`
	ExtraArgs       []string `json:"extra_args"`
	HealthCheckPath string   `json:"health_check_path,omitempty"`
	HealthCheckPort int      `json:"health_check_port,omitempty"`
	Database        string   `json:"database,omitempty"`
	AddonPostgres   bool     `json:"addon_postgres,omitempty"`
}

type DatabaseProvisionRequest struct {
	TenantID string `json:"tenant_id"`
}

type DatabaseInfo struct {
	TenantID  string `json:"tenant_id"`
	Database  string `json:"database"`
	Username  string `json:"username"`
	Password  string `json:"password,omitempty"`
	Host      string `json:"host"`
	Port      int    `json:"port"`
	JdbcURL   string `json:"jdbc_url"`
	Status    string `json:"status"`
	CreatedAt string `json:"created_at"`
}

type TenantResizeRequest struct {
	Tier string `json:"tier"`
}

type NodeRegistrationRequest struct {
	NodeID    string `json:"node_id"`
	DaemonURL string `json:"daemon_url"`
}

type NodeInfo struct {
	NodeID    string `json:"node_id"`
	DaemonURL string `json:"daemon_url"`
	Status    string `json:"status"`
}

type RecoveryResult struct {
	Success   bool   `json:"success"`
	NewPID    int    `json:"new_pid,omitempty"`
	LatencyMs int64  `json:"latency_ms"`
	Error     string `json:"error,omitempty"`
}

type ForkRequest struct {
	TenantID    string   `json:"tenant_id"`
	Tier        string   `json:"tier"`
	JavaVersion string   `json:"java_version"`
	JarPath     string   `json:"jar_path"`
	ExtraArgs   []string `json:"extra_args"`
}

type ForkResponse struct {
	TenantID string `json:"tenant_id"`
	PID      int    `json:"pid"`
	Status   string `json:"status"`
}

type ResizeDaemonRequest struct {
	NewTier string `json:"new_tier"`
}

type ServiceEndpoint struct {
	TenantID   string `json:"tenant_id"`
	NodeID     string `json:"node_id"`
	Host       string `json:"host"`
	Port       int    `json:"port"`
	URL        string `json:"url"`
	HealthPath string `json:"health_path,omitempty"`
	Tier       string `json:"tier"`
	Status     string `json:"status"`
}

type ErrorResponse struct {
	Error string `json:"error"`
}
