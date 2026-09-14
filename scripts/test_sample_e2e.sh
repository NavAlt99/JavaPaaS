#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_DIR"

# Text styles
BOLD='\033[1m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

echo -e "${BOLD}${CYAN}======================================================${NC}"
echo -e "${BOLD}${CYAN}  JavaPaaS End-to-End Sample Application Test Suite   ${NC}"
echo -e "${BOLD}${CYAN}======================================================${NC}\n"

# Ports and Paths
DAEMON_PORT=9199
CONTROLLER_PORT=8089
SAMPLE_PORT=8095
AUTH_TOKEN="test-auth-token-xyz"
TEST_RUN_DIR="/tmp/javapaas-e2e-$$"
CGROUP_DIR="$TEST_RUN_DIR/cgroups"
STATE_FILE="$TEST_RUN_DIR/tenants.json"
DAEMON_URL="http://127.0.0.1:$DAEMON_PORT"
CONTROLLER_URL="http://127.0.0.1:$CONTROLLER_PORT"
SAMPLE_URL="http://127.0.0.1:$SAMPLE_PORT"

mkdir -p "$CGROUP_DIR" "$TEST_RUN_DIR"

cleanup() {
    echo -e "\n${YELLOW}Tearing down background services and child processes...${NC}"
    if [[ -n "${CONTROLLER_PID:-}" ]]; then
        kill "$CONTROLLER_PID" 2>/dev/null || true
    fi
    if [[ -n "${DAEMON_PID:-}" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
    fi
    pkill -f "javapaas-sample-app" 2>/dev/null || true
    rm -rf "$TEST_RUN_DIR"
    echo -e "${GREEN}Cleanup completed.${NC}"
}
trap cleanup EXIT

# 1. Build Sample Application JAR
echo -e "${BOLD}[Step 1/7] Building Sample Application JAR...${NC}"
./sample-app/build.sh
JAR_PATH="$REPO_DIR/sample-app/target/sample-app.jar"
if [[ ! -f "$JAR_PATH" ]]; then
    echo -e "${RED}JAR build failed!${NC}" >&2
    exit 1
fi
echo -e "${GREEN}JAR successfully built at: $JAR_PATH${NC}\n"

# 2. Build Daemon and Controller binaries
echo -e "${BOLD}[Step 2/7] Compiling JavaPaaS Daemon & Controller...${NC}"
cargo build --quiet
(cd paas-controller && go build -o ../target/paas-controller .)
echo -e "${GREEN}Binaries compiled successfully.${NC}\n"

# 3. Launch PaaS Controller and Daemon
echo -e "${BOLD}[Step 3/7] Launching PaaS Controller & Daemon...${NC}"
./target/paas-controller \
    -listen="127.0.0.1:$CONTROLLER_PORT" \
    -daemon-url="$DAEMON_URL" \
    -state-file="$STATE_FILE" \
    -auth-token="$AUTH_TOKEN" > "$TEST_RUN_DIR/controller.log" 2>&1 &
CONTROLLER_PID=$!

CGROUP_ROOT="$CGROUP_DIR" \
LISTEN_ADDR="127.0.0.1:$DAEMON_PORT" \
CONTROLLER_URL="$CONTROLLER_URL" \
AUTH_TOKEN="$AUTH_TOKEN" \
NODE_ID="node-local" \
./target/debug/javapaas-daemon > "$TEST_RUN_DIR/daemon.log" 2>&1 &
DAEMON_PID=$!

echo "Waiting for services to become healthy..."
for i in {1..30}; do
    if curl -s -f "$CONTROLLER_URL/health" >/dev/null && curl -s -f "$DAEMON_URL/health" >/dev/null; then
        echo -e "${GREEN}Controller and Daemon are healthy!${NC}"
        break
    fi
    if [[ $i -eq 30 ]]; then
        echo -e "${RED}Services failed to start!${NC}"
        echo "=== Controller Log ==="; cat "$TEST_RUN_DIR/controller.log"
        echo "=== Daemon Log ==="; cat "$TEST_RUN_DIR/daemon.log"
        exit 1
    fi
    sleep 0.2
done
echo ""

# 4. Register Tenant via Controller API
echo -e "${BOLD}[Step 4/7] Registering sample tenant in PaaS Controller...${NC}"
REGISTER_RESP=$(curl -s -X POST "$CONTROLLER_URL/v1/tenants" \
    -H "Authorization: Bearer $AUTH_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
        \"tenant_id\": \"sample-tenant-1\",
        \"node_id\": \"node-local\",
        \"java_version\": \"21\",
        \"tier\": \"silver\",
        \"jar_path\": \"$JAR_PATH\",
        \"extra_args\": [\"--port=$SAMPLE_PORT\", \"--name=Production-Sample-App\"],
        \"health_check_port\": $SAMPLE_PORT,
        \"health_check_path\": \"/health\"
    }")

echo "Response: $REGISTER_RESP"
echo -e "${GREEN}Tenant spec registered.${NC}\n"

# 5. Start Tenant and Verify Readiness Probing
echo -e "${BOLD}[Step 5/7] Starting Tenant and awaiting readiness probe...${NC}"
START_RESP=$(curl -s -X POST "$CONTROLLER_URL/v1/tenants/sample-tenant-1/start" \
    -H "Authorization: Bearer $AUTH_TOKEN")

echo "Start Response: $START_RESP"
if ! echo "$START_RESP" | grep -q '"success":true'; then
    echo -e "${RED}Failed to start tenant!${NC}"
    echo "=== Daemon Log ==="; cat "$TEST_RUN_DIR/daemon.log"
    exit 1
fi

INITIAL_PID=$(echo "$START_RESP" | grep -o '"new_pid":[0-9]*' | cut -d':' -f2)
echo -e "${GREEN}Tenant running with PID: $INITIAL_PID${NC}"

# Query application directly
APP_HEALTH=$(curl -s "$SAMPLE_URL/health")
echo "Application /health: $APP_HEALTH"
echo -e "${GREEN}Application readiness probe confirmed.${NC}\n"

# 6. Test Live Cgroup Resizing
echo -e "${BOLD}[Step 6/7] Performing live tier resize (silver -> gold)...${NC}"
RESIZE_RESP=$(curl -s -X PUT "$CONTROLLER_URL/v1/tenants/sample-tenant-1/resize" \
    -H "Authorization: Bearer $AUTH_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"tier": "gold"}')

echo "Resize Response: $RESIZE_RESP"
if ! echo "$RESIZE_RESP" | grep -q '"status":"resized"'; then
    echo -e "${RED}Live resize failed!${NC}"
    exit 1
fi
echo -e "${GREEN}Live tier resizing succeeded without process interruption.${NC}\n"

# 7. Test Intentional Crash & Auto-Resurrection
echo -e "${BOLD}[Step 7/7] Triggering crash on sample app to test Auto-Resurrection...${NC}"
CRASH_RESP=$(curl -s -X POST "$SAMPLE_URL/crash" || true)
echo "Crash trigger response: $CRASH_RESP"

echo "Waiting for Daemon Watchdog to detect crash and Controller to resurrect..."
RECOVERED=false
for i in {1..30}; do
    sleep 0.5
    NEW_HEALTH=$(curl -s "$SAMPLE_URL/health" 2>/dev/null || true)
    if [[ -n "$NEW_HEALTH" ]] && echo "$NEW_HEALTH" | grep -q '"status": "UP"'; then
        NEW_PID=$(echo "$NEW_HEALTH" | grep -o '"pid": [0-9]*' | awk '{print $2}')
        if [[ -n "$NEW_PID" && "$NEW_PID" != "$INITIAL_PID" ]]; then
            echo -e "${GREEN}Auto-resurrection succeeded!${NC}"
            echo -e "Initial PID was: ${YELLOW}$INITIAL_PID${NC}, New Resurrected PID is: ${GREEN}$NEW_PID${NC}"
            echo "New Application Health: $NEW_HEALTH"
            RECOVERED=true
            break
        fi
    fi
done

if [[ "$RECOVERED" != "true" ]]; then
    echo -e "${RED}Auto-resurrection failed within timeout!${NC}"
    echo "=== Controller Log ==="; cat "$TEST_RUN_DIR/controller.log"
    echo "=== Daemon Log ==="; cat "$TEST_RUN_DIR/daemon.log"
    exit 1
fi

echo -e "\n${BOLD}${GREEN}======================================================${NC}"
echo -e "${BOLD}${GREEN}  ALL SAMPLE APP E2E TESTS PASSED SUCCESSFULLY!       ${NC}"
echo -e "${BOLD}${GREEN}======================================================${NC}"
