package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestTenantSpecValidate(t *testing.T) {
	validSpec := TenantSpec{
		NodeID:      "node-1",
		JavaVersion: "21",
		Tier:        "gold",
		JarPath:     "/opt/apps/service.jar",
		ExtraArgs:   []string{"-Dfoo=bar"},
	}

	if err := validSpec.Validate(); err != nil {
		t.Fatalf("expected valid spec, got error: %v", err)
	}

	// Missing NodeID
	bad := validSpec
	bad.NodeID = ""
	if err := bad.Validate(); err == nil {
		t.Fatal("expected error for empty node_id")
	}

	// Missing JavaVersion
	bad = validSpec
	bad.JavaVersion = ""
	if err := bad.Validate(); err == nil {
		t.Fatal("expected error for empty java_version")
	}

	// Invalid tier
	bad = validSpec
	bad.Tier = "diamond"
	if err := bad.Validate(); err == nil {
		t.Fatal("expected error for invalid tier")
	}

	// JarPath missing .jar
	bad = validSpec
	bad.JarPath = "/opt/apps/service.war"
	if err := bad.Validate(); err == nil {
		t.Fatal("expected error for non-.jar path")
	}

	// JarPath path traversal
	bad = validSpec
	bad.JarPath = "/opt/apps/../../etc/passwd.jar"
	if err := bad.Validate(); err == nil {
		t.Fatal("expected error for path traversal in jar_path")
	}
}

func TestNodeAffinityStorePersistence(t *testing.T) {
	tmpDir, err := os.MkdirTemp("", "affinity_test_*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tmpDir)

	stateFile := filepath.Join(tmpDir, "tenants.json")
	store1 := NewNodeAffinityStore(stateFile)

	spec := TenantSpec{
		NodeID:      "node-prod-1",
		JavaVersion: "21",
		Tier:        "silver",
		JarPath:     "/opt/apps/app.jar",
	}

	if err := store1.Set("tenant-1", spec); err != nil {
		t.Fatalf("failed to set tenant: %v", err)
	}

	got, ok := store1.Get("tenant-1")
	if !ok || got.NodeID != "node-prod-1" {
		t.Fatalf("expected tenant-1 with node-prod-1, got %+v, ok=%v", got, ok)
	}

	// Verify persistence by loading in a brand new store
	store2 := NewNodeAffinityStore(stateFile)
	got2, ok2 := store2.Get("tenant-1")
	if !ok2 || got2.NodeID != "node-prod-1" {
		t.Fatalf("expected persisted tenant-1 in store2, got %+v, ok=%v", got2, ok2)
	}

	// Test Delete
	if err := store2.Delete("tenant-1"); err != nil {
		t.Fatalf("failed to delete tenant: %v", err)
	}

	store3 := NewNodeAffinityStore(stateFile)
	if _, ok3 := store3.Get("tenant-1"); ok3 {
		t.Fatal("expected tenant-1 to be deleted from store3")
	}
}
