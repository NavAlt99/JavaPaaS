package main

import (
	"encoding/json"
	"log"
	"net/http"
)

type Server struct {
	resurrector *Resurrector
	store       *NodeAffinityStore
	mux         *http.ServeMux
}

func NewServer(resurrector *Resurrector, store *NodeAffinityStore) *Server {
	s := &Server{
		resurrector: resurrector,
		store:       store,
		mux:         http.NewServeMux(),
	}
	s.mux.HandleFunc("POST /v1/internal/recover", s.handleRecover)
	s.mux.HandleFunc("GET /health", s.handleHealth)
	return s
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.mux.ServeHTTP(w, r)
}

func (s *Server) handleRecover(w http.ResponseWriter, r *http.Request) {
	var event TenantCrashEvent
	if err := json.NewDecoder(r.Body).Decode(&event); err != nil {
		http.Error(w, `{"error":"invalid request body"}`, http.StatusBadRequest)
		return
	}

	log.Printf("Recovery request: tenant=%s node=%s reason=%s",
		event.TenantID, event.NodeID, event.Reason)

	result := s.resurrector.resurrect(event)

	w.Header().Set("Content-Type", "application/json")
	if result.Success {
		log.Printf("Recovery succeeded: tenant=%s new_pid=%d latency=%dms",
			event.TenantID, result.NewPID, result.LatencyMs)
		w.WriteHeader(http.StatusOK)
	} else {
		log.Printf("Recovery failed: tenant=%s error=%s", event.TenantID, result.Error)
		w.WriteHeader(http.StatusInternalServerError)
	}
	json.NewEncoder(w).Encode(result)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}
