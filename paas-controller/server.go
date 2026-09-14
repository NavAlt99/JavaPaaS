package main

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strings"
)

type Server struct {
	resurrector  *Resurrector
	store        AffinityStore
	nodeRegistry *NodeRegistry
	authToken    string
	mux          *http.ServeMux
}

func NewServer(resurrector *Resurrector, store AffinityStore, nodeRegistry *NodeRegistry, authToken string) *Server {
	if nodeRegistry == nil {
		nodeRegistry = NewNodeRegistry()
	}
	s := &Server{
		resurrector:  resurrector,
		store:        store,
		nodeRegistry: nodeRegistry,
		authToken:    authToken,
		mux:          http.NewServeMux(),
	}

	s.mux.HandleFunc("POST /v1/internal/recover", s.handleRecover)
	s.mux.HandleFunc("GET /health", s.handleHealth)
	s.mux.HandleFunc("GET /metrics", s.handleMetrics)
	s.mux.HandleFunc("POST /v1/tenants", s.handleRegisterTenant)
	s.mux.HandleFunc("GET /v1/tenants", s.handleListTenants)
	s.mux.HandleFunc("GET /v1/tenants/{id}", s.handleGetTenant)
	s.mux.HandleFunc("PUT /v1/tenants/{id}", s.handleUpdateTenant)
	s.mux.HandleFunc("PUT /v1/tenants/{id}/resize", s.handleResizeTenant)
	s.mux.HandleFunc("PUT /v1/tenants/{id}/tier", s.handleResizeTenant)
	s.mux.HandleFunc("DELETE /v1/tenants/{id}", s.handleDeleteTenant)
	s.mux.HandleFunc("POST /v1/nodes", s.handleRegisterNode)
	s.mux.HandleFunc("GET /v1/nodes", s.handleListNodes)

	return s
}

func (s *Server) checkAuth(r *http.Request) error {
	if s.authToken == "" {
		return nil
	}

	authHdr := r.Header.Get("Authorization")
	if strings.HasPrefix(authHdr, "Bearer ") {
		token := strings.TrimPrefix(authHdr, "Bearer ")
		if strings.TrimSpace(token) == s.authToken {
			return nil
		}
	}

	tokenHdr := r.Header.Get("X-JavaPaaS-Token")
	if strings.TrimSpace(tokenHdr) == s.authToken {
		return nil
	}

	return fmt.Errorf("unauthorized: invalid or missing token")
}

func (s *Server) writeJSON(w http.ResponseWriter, statusCode int, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(statusCode)
	json.NewEncoder(w).Encode(data)
}

func (s *Server) writeError(w http.ResponseWriter, statusCode int, msg string) {
	s.writeJSON(w, statusCode, ErrorResponse{Error: msg})
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/health" && r.URL.Path != "/metrics" {
		if err := s.checkAuth(r); err != nil {
			s.writeError(w, http.StatusUnauthorized, err.Error())
			return
		}
	}
	s.mux.ServeHTTP(w, r)
}

func (s *Server) handleRecover(w http.ResponseWriter, r *http.Request) {
	var event TenantCrashEvent
	if err := json.NewDecoder(r.Body).Decode(&event); err != nil {
		s.writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	if event.TenantID == "" {
		s.writeError(w, http.StatusBadRequest, "tenant_id is required")
		return
	}

	log.Printf("Recovery request: tenant=%s node=%s reason=%s",
		event.TenantID, event.NodeID, event.Reason)

	result := s.resurrector.resurrect(event)

	if result.Success {
		log.Printf("Recovery succeeded: tenant=%s new_pid=%d latency=%dms",
			event.TenantID, result.NewPID, result.LatencyMs)
		s.writeJSON(w, http.StatusOK, result)
	} else {
		log.Printf("Recovery failed: tenant=%s error=%s", event.TenantID, result.Error)
		s.writeJSON(w, http.StatusInternalServerError, result)
	}
}

func (s *Server) handleRegisterTenant(w http.ResponseWriter, r *http.Request) {
	var req TenantRegistrationRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		s.writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	if strings.TrimSpace(req.TenantID) == "" {
		s.writeError(w, http.StatusBadRequest, "tenant_id is required")
		return
	}

	spec := TenantSpec{
		NodeID:          req.NodeID,
		JavaVersion:     req.JavaVersion,
		Tier:            req.Tier,
		JarPath:         req.JarPath,
		ExtraArgs:       req.ExtraArgs,
		HealthCheckPath: req.HealthCheckPath,
		HealthCheckPort: req.HealthCheckPort,
	}

	if err := s.store.Set(req.TenantID, spec); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	s.writeJSON(w, http.StatusCreated, map[string]interface{}{
		"tenant_id": req.TenantID,
		"spec":      spec,
		"status":    "registered",
	})
}

func (s *Server) handleListTenants(w http.ResponseWriter, r *http.Request) {
	tenants := s.store.GetAll()
	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"tenants": tenants,
		"count":   len(tenants),
	})
}

func (s *Server) handleGetTenant(w http.ResponseWriter, r *http.Request) {
	tenantID := r.PathValue("id")
	spec, ok := s.store.Get(tenantID)
	if !ok {
		s.writeError(w, http.StatusNotFound, fmt.Sprintf("tenant %s not found", tenantID))
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"tenant_id": tenantID,
		"spec":      spec,
	})
}

func (s *Server) handleUpdateTenant(w http.ResponseWriter, r *http.Request) {
	tenantID := r.PathValue("id")
	var spec TenantSpec
	if err := json.NewDecoder(r.Body).Decode(&spec); err != nil {
		s.writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	if err := s.store.Set(tenantID, spec); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"tenant_id": tenantID,
		"spec":      spec,
		"status":    "updated",
	})
}

func (s *Server) handleResizeTenant(w http.ResponseWriter, r *http.Request) {
	tenantID := r.PathValue("id")
	spec, ok := s.store.Get(tenantID)
	if !ok {
		s.writeError(w, http.StatusNotFound, fmt.Sprintf("tenant %s not found", tenantID))
		return
	}

	var req TenantResizeRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		s.writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	req.Tier = strings.ToLower(strings.TrimSpace(req.Tier))
	if req.Tier != "silver" && req.Tier != "gold" && req.Tier != "platinum" {
		s.writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid tier '%s': must be silver, gold, or platinum", req.Tier))
		return
	}

	spec.Tier = req.Tier
	if err := s.store.Set(tenantID, spec); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	daemonURL := s.resurrector.resolveDaemonURL(spec.NodeID)
	if err := s.resurrector.ResizeTenant(daemonURL, tenantID, req.Tier); err != nil {
		log.Printf("Notice: live resize call to daemon at %s returned: %v (tenant may not be running)", daemonURL, err)
	}

	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"tenant_id": tenantID,
		"tier":      req.Tier,
		"status":    "resized",
	})
}

func (s *Server) handleDeleteTenant(w http.ResponseWriter, r *http.Request) {
	tenantID := r.PathValue("id")
	if _, ok := s.store.Get(tenantID); !ok {
		s.writeError(w, http.StatusNotFound, fmt.Sprintf("tenant %s not found", tenantID))
		return
	}

	if err := s.store.Delete(tenantID); err != nil {
		s.writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]string{
		"tenant_id": tenantID,
		"status":    "deleted",
	})
}

func (s *Server) handleRegisterNode(w http.ResponseWriter, r *http.Request) {
	var req NodeRegistrationRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		s.writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}
	if strings.TrimSpace(req.NodeID) == "" || strings.TrimSpace(req.DaemonURL) == "" {
		s.writeError(w, http.StatusBadRequest, "node_id and daemon_url are required")
		return
	}

	s.nodeRegistry.Register(req.NodeID, req.DaemonURL)
	s.writeJSON(w, http.StatusCreated, map[string]string{
		"node_id":    req.NodeID,
		"daemon_url": req.DaemonURL,
		"status":     "registered",
	})
}

func (s *Server) handleListNodes(w http.ResponseWriter, r *http.Request) {
	nodes := s.nodeRegistry.GetAll()
	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"nodes": nodes,
		"count": len(nodes),
	})
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	s.writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s *Server) handleMetrics(w http.ResponseWriter, r *http.Request) {
	success, failure, lastLatency := s.resurrector.Stats()
	registered := len(s.store.GetAll())

	w.Header().Set("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
	w.WriteHeader(http.StatusOK)

	fmt.Fprintf(w, "# HELP javapaas_controller_registered_tenants Number of registered tenant specifications\n")
	fmt.Fprintf(w, "# TYPE javapaas_controller_registered_tenants gauge\n")
	fmt.Fprintf(w, "javapaas_controller_registered_tenants %d\n\n", registered)

	fmt.Fprintf(w, "# HELP javapaas_controller_recovery_attempts_total Total recovery attempts by status\n")
	fmt.Fprintf(w, "# TYPE javapaas_controller_recovery_attempts_total counter\n")
	fmt.Fprintf(w, "javapaas_controller_recovery_attempts_total{status=\"success\"} %d\n", success)
	fmt.Fprintf(w, "javapaas_controller_recovery_attempts_total{status=\"failure\"} %d\n\n", failure)

	fmt.Fprintf(w, "# HELP javapaas_controller_last_recovery_latency_ms Latency of most recent recovery in milliseconds\n")
	fmt.Fprintf(w, "# TYPE javapaas_controller_last_recovery_latency_ms gauge\n")
	fmt.Fprintf(w, "javapaas_controller_last_recovery_latency_ms %d\n", lastLatency)
}
