#!/usr/bin/env bash
# Remove the LocalLMOS agent systemd service. Keeps /etc/locallmos-agent
# (credentials + env) unless you pass --purge.
set -euo pipefail

PURGE=0
[[ "${1:-}" == "--purge" ]] && PURGE=1

sudo systemctl disable --now locallmos-agent 2>/dev/null || true
sudo rm -f /etc/systemd/system/locallmos-agent.service
sudo systemctl daemon-reload
sudo rm -f /usr/local/bin/locallmos-agent

if [[ "$PURGE" == "1" ]]; then
  sudo rm -rf /etc/locallmos-agent
  echo "purged /etc/locallmos-agent"
fi
echo "uninstalled locallmos-agent service"
