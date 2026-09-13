package main

type TenantCrashEvent struct {
	TenantID  string `json:"tenant_id"`
	Tier      string `json:"tier"`
	NodeID    string `json:"node_id"`
	Reason    string `json:"reason"`
	ExitCode  *int   `json:"exit_code,omitempty"`
	Timestamp string `json:"timestamp"`
}

type TenantSpec struct {
	NodeID      string   `json:"node_id"`
	JavaVersion string   `json:"java_version"`
	Tier        string   `json:"tier"`
	JarPath     string   `json:"jar_path"`
	ExtraArgs   []string `json:"extra_args"`
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
