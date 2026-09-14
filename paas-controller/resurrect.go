package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type Resurrector struct {
	client           *http.Client
	defaultDaemonURL string
	nodeRegistry     *NodeRegistry
	authToken        string
	store            AffinityStore
	inFlight         sync.Map
	successCount     atomic.Int64
	failureCount     atomic.Int64
	lastLatencyMs    atomic.Int64
}

func NewResurrector(defaultDaemonURL string, authToken string, store AffinityStore, nodeRegistry *NodeRegistry) *Resurrector {
	if nodeRegistry == nil {
		nodeRegistry = NewNodeRegistry()
	}
	return &Resurrector{
		client: &http.Client{
			Timeout: 10 * time.Second,
		},
		defaultDaemonURL: strings.TrimRight(defaultDaemonURL, "/"),
		nodeRegistry:     nodeRegistry,
		authToken:        authToken,
		store:            store,
	}
}

func (r *Resurrector) Stats() (int64, int64, int64) {
	return r.successCount.Load(), r.failureCount.Load(), r.lastLatencyMs.Load()
}

func (r *Resurrector) resolveDaemonURL(nodeID string) string {
	if r.nodeRegistry != nil {
		if u, ok := r.nodeRegistry.Get(nodeID); ok && u != "" {
			return u
		}
	}
	return r.defaultDaemonURL
}

func (r *Resurrector) probeHealth(daemonURL string, port int, path string) error {
	host := "127.0.0.1"
	if u, err := url.Parse(daemonURL); err == nil && u.Hostname() != "" {
		host = u.Hostname()
	}

	cleanPath := path
	if !strings.HasPrefix(cleanPath, "/") {
		cleanPath = "/" + cleanPath
	}

	probeTarget := fmt.Sprintf("http://%s:%d%s", host, port, cleanPath)
	probeClient := &http.Client{Timeout: 1 * time.Second}

	var lastErr error
	for attempt := 0; attempt < 5; attempt++ {
		resp, err := probeClient.Get(probeTarget)
		if err == nil {
			resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return nil
			}
			lastErr = fmt.Errorf("probe returned HTTP %d", resp.StatusCode)
		} else {
			lastErr = err
		}
		time.Sleep(100 * time.Millisecond)
	}

	return fmt.Errorf("health probe failed on %s: %w", probeTarget, lastErr)
}

func (r *Resurrector) stopTenant(daemonURL string, tenantID string) {
	stopURL := fmt.Sprintf("%s/stop/%s", daemonURL, tenantID)
	req, err := http.NewRequestWithContext(context.Background(), http.MethodPost, stopURL, nil)
	if err == nil {
		if r.authToken != "" {
			req.Header.Set("Authorization", "Bearer "+r.authToken)
		}
		if resp, err := r.client.Do(req); err == nil {
			resp.Body.Close()
		}
	}
}

func (r *Resurrector) ResizeTenant(daemonURL string, tenantID string, newTier string) error {
	resizeURL := fmt.Sprintf("%s/resize/%s", daemonURL, tenantID)
	body, _ := json.Marshal(ResizeDaemonRequest{NewTier: newTier})
	req, err := http.NewRequestWithContext(context.Background(), http.MethodPost, resizeURL, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	if r.authToken != "" {
		req.Header.Set("Authorization", "Bearer "+r.authToken)
	}

	resp, err := r.client.Do(req)
	if err != nil {
		return fmt.Errorf("daemon resize call failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("daemon returned HTTP %d during resize", resp.StatusCode)
	}
	return nil
}

func (r *Resurrector) resurrect(event TenantCrashEvent) RecoveryResult {
	start := time.Now()

	// De-duplicate concurrent recovery requests for the same tenant
	if _, loaded := r.inFlight.LoadOrStore(event.TenantID, true); loaded {
		r.failureCount.Add(1)
		r.lastLatencyMs.Store(time.Since(start).Milliseconds())
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("recovery already in progress for tenant %s", event.TenantID),
		}
	}
	defer r.inFlight.Delete(event.TenantID)

	spec, ok := r.store.Get(event.TenantID)
	if !ok {
		r.failureCount.Add(1)
		r.lastLatencyMs.Store(time.Since(start).Milliseconds())
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("no affinity record for tenant %s", event.TenantID),
		}
	}

	daemonURL := r.resolveDaemonURL(spec.NodeID)

	healthURL := fmt.Sprintf("%s/health", daemonURL)
	healthReq, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, healthURL, nil)
	if r.authToken != "" {
		healthReq.Header.Set("Authorization", "Bearer "+r.authToken)
	}

	resp, err := r.client.Do(healthReq)
	if err != nil {
		r.failureCount.Add(1)
		r.lastLatencyMs.Store(time.Since(start).Milliseconds())
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("daemon health check failed on %s: %v", daemonURL, err),
		}
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		r.failureCount.Add(1)
		r.lastLatencyMs.Store(time.Since(start).Milliseconds())
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("daemon unhealthy on %s: HTTP %d", daemonURL, resp.StatusCode),
		}
	}

	forkReq := ForkRequest{
		TenantID:    event.TenantID,
		Tier:        spec.Tier,
		JavaVersion: spec.JavaVersion,
		JarPath:     spec.JarPath,
		ExtraArgs:   spec.ExtraArgs,
	}

	body, err := json.Marshal(forkReq)
	if err != nil {
		r.failureCount.Add(1)
		r.lastLatencyMs.Store(time.Since(start).Milliseconds())
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("marshal fork request: %v", err),
		}
	}

	var forkResp ForkResponse
	var lastErr error
	for attempt := 0; attempt < 3; attempt++ {
		forkURL := fmt.Sprintf("%s/fork", daemonURL)
		httpReq, err := http.NewRequestWithContext(context.Background(), http.MethodPost, forkURL, bytes.NewReader(body))
		if err != nil {
			lastErr = err
			break
		}
		httpReq.Header.Set("Content-Type", "application/json")
		if r.authToken != "" {
			httpReq.Header.Set("Authorization", "Bearer "+r.authToken)
		}

		resp, err := r.client.Do(httpReq)
		if err != nil {
			lastErr = fmt.Errorf("POST /fork attempt %d: %w", attempt+1, err)
			time.Sleep(time.Duration(100*(1<<attempt)) * time.Millisecond)
			continue
		}

		if resp.StatusCode != http.StatusOK {
			lastErr = fmt.Errorf("daemon returned HTTP %d on attempt %d", resp.StatusCode, attempt+1)
			resp.Body.Close()
			time.Sleep(time.Duration(100*(1<<attempt)) * time.Millisecond)
			continue
		}

		if err := json.NewDecoder(resp.Body).Decode(&forkResp); err != nil {
			lastErr = fmt.Errorf("decode fork response attempt %d: %w", attempt+1, err)
			resp.Body.Close()
			time.Sleep(time.Duration(100*(1<<attempt)) * time.Millisecond)
			continue
		}
		resp.Body.Close()

		// Application Health Probing
		if spec.HealthCheckPath != "" && spec.HealthCheckPort > 0 {
			if probeErr := r.probeHealth(daemonURL, spec.HealthCheckPort, spec.HealthCheckPath); probeErr != nil {
				log.Printf("Readiness probe failed for tenant %s after fork: %v", event.TenantID, probeErr)
				r.stopTenant(daemonURL, event.TenantID)
				lastErr = probeErr
				time.Sleep(time.Duration(100*(1<<attempt)) * time.Millisecond)
				continue
			}
		}

		latency := time.Since(start).Milliseconds()
		r.successCount.Add(1)
		r.lastLatencyMs.Store(latency)

		log.Printf("Resurrected tenant %s on node %s with new PID %d (latency: %dms)",
			event.TenantID, spec.NodeID, forkResp.PID, latency)

		return RecoveryResult{
			Success:   true,
			NewPID:    forkResp.PID,
			LatencyMs: latency,
		}
	}

	latency := time.Since(start).Milliseconds()
	r.failureCount.Add(1)
	r.lastLatencyMs.Store(latency)

	return RecoveryResult{
		Success:   false,
		LatencyMs: latency,
		Error:     fmt.Sprintf("fork retry exhausted: %v", lastErr),
	}
}
