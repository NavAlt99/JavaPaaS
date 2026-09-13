#!/bin/bash
set -euo pipefail

BIN_DIR="/opt/javapaas/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building javapaas components..."

cd "$PROJECT_DIR"

mkdir -p "$BIN_DIR"

echo "Building Rust daemon..."
. "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release 2>&1
cp target/release/javapaas-daemon "$BIN_DIR/"

echo "Building Go controller..."
cd paas-controller
go build -o "$BIN_DIR/paas-controller" .
cd ..

cat > /etc/systemd/system/javapaas-daemon.service <<'UNIT'
[Unit]
Description=JavaPaaS Rust Daemon
After=network.target

[Service]
Type=simple
User=root
ExecStart=/opt/javapaas/bin/javapaas-daemon
Environment=RUST_LOG=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

cat > /etc/systemd/system/javapaas-controller.service <<'UNIT'
[Unit]
Description=JavaPaaS Go Controller
After=network.target javapaas-daemon.service

[Service]
Type=simple
User=root
ExecStart=/opt/javapaas/bin/paas-controller -daemon-url http://localhost:9100
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT

echo "Build complete. Binaries at $BIN_DIR"
echo "Run 'systemctl daemon-reload && systemctl enable --now javapaas-daemon javapaas-controller' to start"
