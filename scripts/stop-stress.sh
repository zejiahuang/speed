#!/usr/bin/env bash
#
# Stop anything left over from a smoke or stress run and remove the interface.
#
# A script file rather than an inline `bash -lc` because `pkill -f <pattern>`
# matches the command line of the shell running it: passing the pattern inline
# means the shell kills itself before it finishes cleaning up.
#
#   sudo bash scripts/stop-stress.sh

set -uo pipefail

if [ "$(id -u)" -ne 0 ]; then
  exec sudo env "PATH=$PATH" bash "$0"
fi

for pattern in tun_smoke_client.py tun_smoke_servers.py tun-stress.sh tun-smoke.sh; do
  pkill -f "$pattern" 2>/dev/null
done
pkill -x tun_stress 2>/dev/null
pkill -x tun_smoke 2>/dev/null

sleep 1

ip route del 203.0.113.0/24 dev watt0 2>/dev/null
ip route del 198.51.100.0/24 dev watt0 2>/dev/null
ip link del watt0 2>/dev/null

echo "remaining:"
ps -eo pid,comm,args | grep -E 'tun_stress|tun_smoke' | grep -v grep || echo "  (none)"
echo "interfaces:"
ip -brief addr show watt0 2>/dev/null || echo "  (watt0 gone)"
