package main

import (
	"context"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"
)

func main() {
	defaultStateFile := os.Getenv("STATE_FILE")
	if defaultStateFile == "" {
		defaultStateFile = "/opt/javapaas/data/tenants.json"
	}

	defaultAuthToken := os.Getenv("AUTH_TOKEN")

	listenAddr := flag.String("listen", "127.0.0.1:8080", "Controller HTTP listen address")
	daemonURL := flag.String("daemon-url", "http://127.0.0.1:9100", "Rust daemon base URL")
	stateFile := flag.String("state-file", defaultStateFile, "Path to persistent tenant affinity JSON file")
	authToken := flag.String("auth-token", defaultAuthToken, "Internal bearer auth token")
	flag.Parse()

	nodeRegistry := NewNodeRegistry()
	store := NewNodeAffinityStore(*stateFile)
	resurrector := NewResurrector(*daemonURL, *authToken, store, nodeRegistry)
	server := NewServer(resurrector, store, nodeRegistry, *authToken)

	srv := &http.Server{
		Addr:    *listenAddr,
		Handler: server,
	}

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		sig := <-sigCh
		log.Printf("Received signal %s, shutting down controller gracefully...", sig)

		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()

		if err := srv.Shutdown(ctx); err != nil {
			log.Printf("Controller graceful shutdown failed: %v", err)
		}
	}()

	log.Printf("Starting paas-controller on %s (daemon: %s, state: %s)", *listenAddr, *daemonURL, *stateFile)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatalf("Server failed: %v", err)
	}
	log.Println("Controller stopped cleanly")
}
