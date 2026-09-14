package main

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestDBaaSManagerLifecycle(t *testing.T) {
	mgr := NewDBaaSManager(nil, "db.internal.paas", 5432)

	// 1. Provision database for tenant
	tenantID := "customer-billing-service"
	dbInfo, err := mgr.Provision(tenantID)
	if err != nil {
		t.Fatalf("Provision failed: %v", err)
	}

	if dbInfo.TenantID != tenantID {
		t.Errorf("expected tenant_id %s, got %s", tenantID, dbInfo.TenantID)
	}
	if !strings.HasPrefix(dbInfo.Database, "tenant_customer_billing_service_db") {
		t.Errorf("unexpected database name: %s", dbInfo.Database)
	}
	if !strings.HasPrefix(dbInfo.Username, "tenant_customer_billing_service_usr") {
		t.Errorf("unexpected username: %s", dbInfo.Username)
	}
	if !strings.HasPrefix(dbInfo.JdbcURL, "jdbc:postgresql://db.internal.paas:5432/") {
		t.Errorf("unexpected jdbc url: %s", dbInfo.JdbcURL)
	}
	if dbInfo.Password == "" {
		t.Errorf("expected generated password to be non-empty")
	}

	// 2. Idempotency check: provisioning same tenant returns same DB
	dbInfo2, err := mgr.Provision(tenantID)
	if err != nil {
		t.Fatalf("Idempotent provision failed: %v", err)
	}
	if dbInfo2.Database != dbInfo.Database || dbInfo2.Password != dbInfo.Password {
		t.Errorf("idempotent call did not return matching credentials")
	}

	// 3. Retrieve database
	retrieved, ok := mgr.Get(tenantID)
	if !ok || retrieved.Database != dbInfo.Database {
		t.Fatalf("failed to retrieve database for %s", tenantID)
	}

	// 4. List databases
	all := mgr.List()
	if len(all) != 1 {
		t.Errorf("expected 1 database in list, got %d", len(all))
	}

	// 5. Inject JVM arguments
	baseArgs := []string{"--server.port=8080"}
	injected, info, err := mgr.InjectDatabaseArgs(tenantID, baseArgs)
	if err != nil {
		t.Fatalf("InjectDatabaseArgs failed: %v", err)
	}
	if info.Database != dbInfo.Database {
		t.Errorf("mismatched database info returned from inject")
	}

	hasSpringURL := false
	hasSpringUser := false
	hasSpringPass := false
	for _, arg := range injected {
		if strings.HasPrefix(arg, "-Dspring.datasource.url=") {
			hasSpringURL = true
		}
		if strings.HasPrefix(arg, "-Dspring.datasource.username=") {
			hasSpringUser = true
		}
		if strings.HasPrefix(arg, "-Dspring.datasource.password=") {
			hasSpringPass = true
		}
	}
	if !hasSpringURL || !hasSpringUser || !hasSpringPass {
		t.Errorf("missing injected Spring datasource properties in args: %v", injected)
	}

	// 6. Deprovision
	if err := mgr.Deprovision(tenantID); err != nil {
		t.Fatalf("Deprovision failed: %v", err)
	}
	if _, ok := mgr.Get(tenantID); ok {
		t.Errorf("database still found after deprovisioning")
	}
}

func TestServerDBaaSEndpoints(t *testing.T) {
	store := NewNodeAffinityStore("")
	nodes := NewNodeRegistry()
	resurrector := NewResurrector("http://localhost:9100", "", store, nodes)
	dbaas := NewDBaaSManager(nil, "localhost", 5432)
	server := NewServer(resurrector, store, nodes, dbaas, "")

	// 1. Provision database via POST /v1/databases
	body, _ := json.Marshal(DatabaseProvisionRequest{TenantID: "tenant-cart"})
	req := httptest.NewRequest(http.MethodPost, "/v1/databases", bytes.NewReader(body))
	w := httptest.NewRecorder()
	server.ServeHTTP(w, req)

	if w.Code != http.StatusCreated {
		t.Fatalf("expected 201 for POST /v1/databases, got %d: %s", w.Code, w.Body.String())
	}

	var dbInfo DatabaseInfo
	if err := json.NewDecoder(w.Body).Decode(&dbInfo); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if dbInfo.TenantID != "tenant-cart" {
		t.Errorf("expected tenant_id 'tenant-cart', got %s", dbInfo.TenantID)
	}

	// 2. Query database via GET /v1/databases/tenant-cart
	req = httptest.NewRequest(http.MethodGet, "/v1/databases/tenant-cart", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for GET /v1/databases/tenant-cart, got %d", w.Code)
	}

	// 3. List databases via GET /v1/databases
	req = httptest.NewRequest(http.MethodGet, "/v1/databases", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 for GET /v1/databases, got %d", w.Code)
	}

	// 4. Test Automatic DBaaS Injection on Tenant Registration
	tenantReq := TenantRegistrationRequest{
		TenantID:      "tenant-auto-db",
		NodeID:        "node-1",
		JavaVersion:   "21",
		Tier:          "silver",
		JarPath:       "/opt/apps/service.jar",
		AddonPostgres: true,
	}
	reqBytes, _ := json.Marshal(tenantReq)
	req = httptest.NewRequest(http.MethodPost, "/v1/tenants", bytes.NewReader(reqBytes))
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)

	if w.Code != http.StatusCreated {
		t.Fatalf("expected 201 for tenant registration with DB add-on, got %d: %s", w.Code, w.Body.String())
	}

	spec, ok := store.Get("tenant-auto-db")
	if !ok {
		t.Fatalf("expected tenant spec to be saved")
	}
	if spec.Database == "" {
		t.Errorf("expected spec.Database to be set")
	}

	hasJDBCURL := false
	for _, arg := range spec.ExtraArgs {
		if strings.HasPrefix(arg, "-Dspring.datasource.url=") {
			hasJDBCURL = true
		}
	}
	if !hasJDBCURL {
		t.Errorf("expected -Dspring.datasource.url in tenant extra args, got: %v", spec.ExtraArgs)
	}

	// 5. Delete tenant should also deprovision its database
	req = httptest.NewRequest(http.MethodDelete, "/v1/tenants/tenant-auto-db", nil)
	w = httptest.NewRecorder()
	server.ServeHTTP(w, req)
	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 on tenant delete, got %d", w.Code)
	}

	if _, found := dbaas.Get("tenant-auto-db"); found {
		t.Errorf("expected database to be deprovisioned when tenant was deleted")
	}
}
