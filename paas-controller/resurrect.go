package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"time"
)

type Resurrector struct {
	client    *http.Client
	daemonURL string
	store     *NodeAffinityStore
}

func NewResurrector(daemonURL string, store *NodeAffinityStore) *Resurrector {
	return &Resurrector{
		client: &http.Client{
			Timeout: 10 * time.Second,
		},
		daemonURL: daemonURL,
		store:     store,
	}
}

func (r *Resurrector) resurrect(event TenantCrashEvent) RecoveryResult {
	start := time.Now()

	spec, ok := r.store.Get(event.TenantID)
	if !ok {
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("no affinity record for tenant %s", event.TenantID),
		}
	}

	healthURL := fmt.Sprintf("%s/health", r.daemonURL)
	resp, err := r.client.Get(healthURL)
	if err != nil {
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("daemon health check failed: %v", err),
		}
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("daemon unhealthy: HTTP %d", resp.StatusCode),
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
		return RecoveryResult{
			Success:   false,
			LatencyMs: time.Since(start).Milliseconds(),
			Error:     fmt.Sprintf("marshal fork request: %v", err),
		}
	}

	var forkResp ForkResponse
	var lastErr error
	for attempt := 0; attempt < 3; attempt++ {
		forkURL := fmt.Sprintf("%s/fork", r.daemonURL)
		resp, err := r.client.Post(forkURL, "application/json", bytes.NewReader(body))
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

		log.Printf("Resurrected tenant %s on node %s with new PID %d",
			event.TenantID, spec.NodeID, forkResp.PID)

		return RecoveryResult{
			Success:   true,
			NewPID:    forkResp.PID,
			LatencyMs: time.Since(start).Milliseconds(),
		}
	}

	return RecoveryResult{
		Success:   false,
		LatencyMs: time.Since(start).Milliseconds(),
		Error:     fmt.Sprintf("fork retry exhausted: %v", lastErr),
	}
}
