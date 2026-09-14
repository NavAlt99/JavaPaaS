package main

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestServerHealthAndAuth(t *testing.T) {
	store := NewNodeAffinityStore("")
	nodes := NewNodeRegistry()
	resurrector := NewResurrector("http://localhost:9100", "test-secret", store, nodes)
	server := NewServer(resurrector, store, nodes, "test-secret")

	// /health should be accessible without auth
	req := httptest.NewRequest(http.MethodGet, "/health", nil)
	w := httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for /health, got %d", w.Code)
	}

	// Protected endpoint without auth should return 401
	req = httptest.NewRequest(http.MethodGet, "/v1/tenants", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 for /v1/tenants without token, got %d", w.Code)
	}

	// Protected endpoint with valid Bearer token should succeed
	req = httptest.NewRequest(http.MethodGet, "/v1/tenants", nil)
	req.Header.Set("Authorization", "Bearer test-secret")
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for /v1/tenants with valid token, got %d", w.Code)
	}

	// /metrics should be accessible without auth and return Prometheus format
	req = httptest.NewRequest(http.MethodGet, "/metrics", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for /metrics, got %d", w.Code)
	}
	if !bytes.Contains(w.Body.Bytes(), []byte("javapaas_controller_registered_tenants")) {
		t.Fatalf("expected Prometheus metric in body, got: %s", w.Body.String())
	}
}

func TestServerTenantCRUD(t *testing.T) {
	store := NewNodeAffinityStore("")
	nodes := NewNodeRegistry()
	resurrector := NewResurrector("http://localhost:9100", "", store, nodes)
	server := NewServer(resurrector, store, nodes, "")

	// 1. Create tenant
	regReq := TenantRegistrationRequest{
		TenantID:        "customer-42",
		NodeID:          "node-east-1",
		JavaVersion:     "21",
		Tier:            "gold",
		JarPath:         "/opt/apps/backend.jar",
		ExtraArgs:       []string{"-Denv=prod"},
		HealthCheckPath: "/health",
		HealthCheckPort: 8081,
	}
	body, _ := json.Marshal(regReq)

	req := httptest.NewRequest(http.MethodPost, "/v1/tenants", bytes.NewReader(body))
	w := httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusCreated {
		t.Fatalf("expected 201 Created, got %d: %s", w.Code, w.Body.String())
	}

	// 2. Get tenant
	req = httptest.NewRequest(http.MethodGet, "/v1/tenants/customer-42", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 OK, got %d", w.Code)
	}

	// 3. Resize tenant (PUT /v1/tenants/{id}/resize)
	resizeReq := TenantResizeRequest{Tier: "platinum"}
	resizeBody, _ := json.Marshal(resizeReq)
	req = httptest.NewRequest(http.MethodPut, "/v1/tenants/customer-42/resize", bytes.NewReader(resizeBody))
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 OK for resize, got %d: %s", w.Code, w.Body.String())
	}
	updatedSpec, _ := store.Get("customer-42")
	if updatedSpec.Tier != "platinum" {
		t.Fatalf("expected tier to be updated to platinum, got %s", updatedSpec.Tier)
	}

	// 4. Node registration (POST /v1/nodes and GET /v1/nodes)
	nodeReq := NodeRegistrationRequest{
		NodeID:    "node-east-1",
		DaemonURL: "http://10.0.0.1:9100",
	}
	nodeBody, _ := json.Marshal(nodeReq)
	req = httptest.NewRequest(http.MethodPost, "/v1/nodes", bytes.NewReader(nodeBody))
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusCreated {
		t.Fatalf("expected 201 for node registration, got %d", w.Code)
	}

	req = httptest.NewRequest(http.MethodGet, "/v1/nodes", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for node listing, got %d", w.Code)
	}

	// 5. Delete tenant
	req = httptest.NewRequest(http.MethodDelete, "/v1/tenants/customer-42", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 OK, got %d", w.Code)
	}

	// 6. Get after delete should return 404
	req = httptest.NewRequest(http.MethodGet, "/v1/tenants/customer-42", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusNotFound {
		t.Fatalf("expected 404 Not Found, got %d", w.Code)
	}
}
