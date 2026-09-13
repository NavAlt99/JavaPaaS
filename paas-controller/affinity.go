package main

import "sync"

type NodeAffinityStore struct {
	mu   sync.RWMutex
	data map[string]TenantSpec
}

func NewNodeAffinityStore() *NodeAffinityStore {
	return &NodeAffinityStore{
		data: make(map[string]TenantSpec),
	}
}

func (s *NodeAffinityStore) Get(tenantID string) (TenantSpec, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	spec, ok := s.data[tenantID]
	return spec, ok
}

func (s *NodeAffinityStore) Set(tenantID string, spec TenantSpec) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.data[tenantID] = spec
}

func (s *NodeAffinityStore) Delete(tenantID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.data, tenantID)
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
