package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
)

func TestResurrectorMissingAffinity(t *testing.T) {
	store := NewNodeAffinityStore("")
	nodes := NewNodeRegistry()
	r := NewResurrector("http://localhost:9100", "", store, nodes)

	res := r.resurrect(TenantCrashEvent{
		TenantID: "unknown-tenant",
		Tier:     "gold",
		NodeID:   "node-1",
		Reason:   "EXIT",
	})

	if res.Success {
		t.Fatal("expected recovery failure for missing tenant affinity")
	}
	if res.Error == "" {
		t.Fatal("expected non-empty error message")
	}
}

func TestResurrectorSuccessfulRecovery(t *testing.T) {
	var forkCalls atomic.Int32

	mockDaemon := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/health":
			w.WriteHeader(http.StatusOK)
			w.Write([]byte(`{"status":"ok"}`))
		case "/fork":
			forkCalls.Add(1)
			var fReq ForkRequest
			json.NewDecoder(req.Body).Decode(&fReq)
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(ForkResponse{
				TenantID: fReq.TenantID,
				PID:      9876,
				Status:   "running",
			})
		default:
			http.NotFound(w, req)
		}
	}))
	defer mockDaemon.Close()

	store := NewNodeAffinityStore("")
	store.Set("tenant-99", TenantSpec{
		NodeID:      "node-1",
		JavaVersion: "21",
		Tier:        "gold",
		JarPath:     "/opt/apps/app.jar",
	})

	nodes := NewNodeRegistry()
	nodes.Register("node-1", mockDaemon.URL)

	r := NewResurrector(mockDaemon.URL, "auth-token", store, nodes)

	res := r.resurrect(TenantCrashEvent{
		TenantID: "tenant-99",
		Tier:     "gold",
		NodeID:   "node-1",
		Reason:   "OOM_KILL",
	})

	if !res.Success {
		t.Fatalf("expected successful recovery, got error: %s", res.Error)
	}
	if res.NewPID != 9876 {
		t.Fatalf("expected new PID 9876, got %d", res.NewPID)
	}
	if forkCalls.Load() != 1 {
		t.Fatalf("expected exactly 1 fork call, got %d", forkCalls.Load())
	}
}

func TestResurrectorHealthProbing(t *testing.T) {
	var stopCalls atomic.Int32

	mockDaemon := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/health":
			w.WriteHeader(http.StatusOK)
		case "/fork":
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(ForkResponse{
				TenantID: "probe-tenant",
				PID:      5555,
				Status:   "running",
			})
		case "/stop/probe-tenant":
			stopCalls.Add(1)
			w.WriteHeader(http.StatusOK)
		}
	}))
	defer mockDaemon.Close()

	store := NewNodeAffinityStore("")
	// Health check pointing to an invalid/non-running port 59999
	store.Set("probe-tenant", TenantSpec{
		NodeID:          "node-1",
		JavaVersion:     "21",
		Tier:            "silver",
		JarPath:         "/opt/apps/app.jar",
		HealthCheckPath: "/actuator/health",
		HealthCheckPort: 59999,
	})

	nodes := NewNodeRegistry()
	nodes.Register("node-1", mockDaemon.URL)
	r := NewResurrector(mockDaemon.URL, "", store, nodes)

	res := r.resurrect(TenantCrashEvent{
		TenantID: "probe-tenant",
		Tier:     "silver",
		NodeID:   "node-1",
		Reason:   "EXIT",
	})

	// Probe should fail and trigger rollback stop
	if res.Success {
		t.Fatal("expected recovery failure when health probe fails")
	}
	if stopCalls.Load() == 0 {
		t.Fatal("expected rollback stop call when health check fails")
	}
}

func TestResurrectorConcurrentDeduplication(t *testing.T) {
	mockDaemon := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/health":
			w.WriteHeader(http.StatusOK)
		case "/fork":
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(ForkResponse{
				TenantID: "tenant-dedup",
				PID:      1234,
				Status:   "running",
			})
		}
	}))
	defer mockDaemon.Close()

	store := NewNodeAffinityStore("")
	store.Set("tenant-dedup", TenantSpec{
		NodeID:      "node-1",
		JavaVersion: "21",
		Tier:        "silver",
		JarPath:     "/opt/apps/app.jar",
	})

	r := NewResurrector(mockDaemon.URL, "", store, nil)

	// Simulate concurrent recovery calls
	var wg sync.WaitGroup
	results := make([]RecoveryResult, 2)

	for i := 0; i < 2; i++ {
		idx := i
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[idx] = r.resurrect(TenantCrashEvent{
				TenantID: "tenant-dedup",
				Tier:     "silver",
				NodeID:   "node-1",
				Reason:   "OOM_KILL",
			})
		}()
	}
	wg.Wait()

	successCount := 0
	for _, res := range results {
		if res.Success {
			successCount++
		}
	}

	if successCount == 0 {
		t.Fatal("expected at least 1 recovery to succeed")
	}
}
