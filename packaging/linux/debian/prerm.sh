#!/bin/sh
# prerm — stop + disable the fleet services on package removal. Leaves /var/lib/ce (identity, chain,
# wallet) in place so a reinstall keeps the node's identity and enrollment; purge removes it.
set -eu

if command -v systemctl >/dev/null 2>&1; then
  for svc in ce-infer-worker.service ce-enroll.service ce-node.service; do
    systemctl stop "$svc" 2>/dev/null || true
    systemctl disable "$svc" 2>/dev/null || true
  done
  systemctl daemon-reload || true
fi

# Tear down the egress firewall table (best-effort; leave other firewall rules untouched).
if command -v nft >/dev/null 2>&1; then
  nft delete table inet ce_fleet 2>/dev/null || true
fi

echo "ce-fleet: services stopped; /var/lib/ce preserved (purge to remove identity + chain)"
exit 0
