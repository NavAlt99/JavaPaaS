package main

import (
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
)

func main() {
	listenAddr := flag.String("listen", ":8080", "Controller HTTP listen address")
	daemonURL := flag.String("daemon-url", "http://localhost:9100", "Rust daemon base URL")
	flag.Parse()

	store := NewNodeAffinityStore()
	resurrector := NewResurrector(*daemonURL, store)
	server := NewServer(resurrector, store)

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		<-sigCh
		log.Println("Shutting down controller...")
		os.Exit(0)
	}()

	log.Printf("Starting paas-controller on %s (daemon: %s)", *listenAddr, *daemonURL)
	if err := http.ListenAndServe(*listenAddr, server); err != nil {
		log.Fatalf("Server failed: %v", err)
	}
}
