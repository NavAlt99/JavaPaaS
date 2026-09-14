package main

import (
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

type AffinityStore interface {
	Get(tenantID string) (TenantSpec, bool)
	Set(tenantID string, spec TenantSpec) error
	Delete(tenantID string) error
	GetAll() map[string]TenantSpec
}

type NodeAffinityStore struct {
	mu            sync.RWMutex
	data          map[string]TenantSpec
	stateFilePath string
}

func NewNodeAffinityStore(stateFilePath string) *NodeAffinityStore {
	store := &NodeAffinityStore{
		data:          make(map[string]TenantSpec),
		stateFilePath: stateFilePath,
	}

	if stateFilePath != "" {
		if err := store.load(); err != nil {
			log.Printf("Warning: failed to load affinity store from %s: %v", stateFilePath, err)
		} else {
			log.Printf("Loaded %d tenant specs from %s", len(store.data), stateFilePath)
		}
	}

	return store
}

func (s *NodeAffinityStore) load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, err := os.Stat(s.stateFilePath); os.IsNotExist(err) {
		return nil
	}

	raw, err := os.ReadFile(s.stateFilePath)
	if err != nil {
		return err
	}

	var data map[string]TenantSpec
	if err := json.Unmarshal(raw, &data); err != nil {
		return err
	}

	s.data = data
	return nil
}

func (s *NodeAffinityStore) saveLocked() error {
	if s.stateFilePath == "" {
		return nil
	}

	raw, err := json.MarshalIndent(s.data, "", "  ")
	if err != nil {
		return err
	}

	dir := filepath.Dir(s.stateFilePath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}

	tmpFile := fmt.Sprintf("%s.tmp.%d", s.stateFilePath, os.Getpid())
	if err := os.WriteFile(tmpFile, raw, 0644); err != nil {
		return err
	}

	return os.Rename(tmpFile, s.stateFilePath)
}

func (s *NodeAffinityStore) Get(tenantID string) (TenantSpec, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	spec, ok := s.data[tenantID]
	return spec, ok
}

func (s *NodeAffinityStore) Set(tenantID string, spec TenantSpec) error {
	if err := spec.Validate(); err != nil {
		return err
	}
	if strings.TrimSpace(tenantID) == "" {
		return fmt.Errorf("tenantID cannot be empty")
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	s.data[tenantID] = spec
	return s.saveLocked()
}

func (s *NodeAffinityStore) Delete(tenantID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.data, tenantID)
	return s.saveLocked()
}

func (s *NodeAffinityStore) GetAll() map[string]TenantSpec {
	s.mu.RLock()
	defer s.mu.RUnlock()
	result := make(map[string]TenantSpec, len(s.data))
	for k, v := range s.data {
		result[k] = v
	}
	return result
}

// NodeRegistry maintains cluster node mapping for multi-node daemon orchestration
type NodeRegistry struct {
	mu    sync.RWMutex
	nodes map[string]string // nodeID -> daemonURL
}

func NewNodeRegistry() *NodeRegistry {
	return &NodeRegistry{
		nodes: make(map[string]string),
	}
}

func (r *NodeRegistry) Register(nodeID string, daemonURL string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.nodes[nodeID] = strings.TrimRight(daemonURL, "/")
}

func (r *NodeRegistry) Get(nodeID string) (string, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	url, ok := r.nodes[nodeID]
	return url, ok
}

func (r *NodeRegistry) GetAll() map[string]string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	copyMap := make(map[string]string, len(r.nodes))
	for k, v := range r.nodes {
		copyMap[k] = v
	}
	return copyMap
}
