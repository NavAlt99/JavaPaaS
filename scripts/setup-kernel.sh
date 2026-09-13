#!/bin/bash
set -euo pipefail

echo "Applying kernel parameters for javapaas..."

sysctl -w vm.swappiness=0
sysctl -w vm.overcommit_memory=1
sysctl -w kernel.pid_max=4194304
sysctl -w fs.file-max=2097152

cat > /etc/sysctl.d/99-javapaas.conf <<'EOF'
vm.swappiness=0
vm.overcommit_memory=1
kernel.pid_max=4194304
fs.file-max=2097152
EOF

echo "Kernel parameters applied and persisted"
