#!/bin/sh
# postinst — create the ce system user + data dir, pin the org root pubkey, enable services + egress
# firewall. Idempotent: safe to re-run on upgrade. The org root key is the AUTHORIZATION pin;
# placing it here (file placement) confers no authority — only the org root's signed caps do.
set -eu

CE_USER=ce
CE_DATA=/var/lib/ce

# 1. system user (no shell, no home login) — the node runs as `ce`, least privilege.
if ! id "$CE_USER" >/dev/null 2>&1; then
  useradd --system --home-dir "$CE_DATA" --shell /usr/sbin/nologin "$CE_USER" 2>/dev/null \
    || adduser --system --no-create-home --home "$CE_DATA" "$CE_USER" 2>/dev/null \
    || true
fi

# 2. data dir, chmod 700, owned by ce.
mkdir -p "$CE_DATA/roots" "$CE_DATA/wallet"
chown -R "$CE_USER":"$CE_USER" "$CE_DATA" 2>/dev/null || true
chmod 700 "$CE_DATA"

# 3. pin the org PUBLIC root key into the node's accepted roots, if the installer dropped it.
#    (Machine-targeted deploy tooling places /etc/ce/ce-root.pub; Ansible/MECM/Intune own this.)
if [ -f /etc/ce/ce-root.pub ]; then
  cp /etc/ce/ce-root.pub "$CE_DATA/roots/ce-root.pub"
  chown "$CE_USER":"$CE_USER" "$CE_DATA/roots/ce-root.pub" 2>/dev/null || true
  chmod 644 "$CE_DATA/roots/ce-root.pub"
  echo "ce-fleet: org root pubkey pinned to $CE_DATA/roots/ce-root.pub"
else
  echo "ce-fleet: WARNING no /etc/ce/ce-root.pub present — drop the org PUBLIC root key there so this node honors org capabilities" >&2
fi

# 4. egress-deny firewall (the air-gap's second half).
if command -v nft >/dev/null 2>&1; then
  nft -f /etc/ce/ce-fleet-egress.nft 2>/dev/null \
    && echo "ce-fleet: egress firewall applied" \
    || echo "ce-fleet: WARNING could not apply egress firewall (nft -f failed)" >&2
fi

# 5. open the libp2p LAN port (TCP+UDP 4001) for those sites using ufw instead of raw nft.
if command -v ufw >/dev/null 2>&1; then
  ufw allow 4001/tcp >/dev/null 2>&1 || true
  ufw allow 4001/udp >/dev/null 2>&1 || true
fi

# 6. enable + start the services. ce-enroll is oneshot (Before=ce-infer-worker); enabling the
#    worker pulls it in. The node starts immediately; enrollment runs once the delegate is reachable.
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
  systemctl enable --now ce-node.service 2>/dev/null || true
  systemctl enable ce-enroll.service 2>/dev/null || true
  systemctl enable --now ce-infer-worker.service 2>/dev/null || true
  echo "ce-fleet: services enabled (ce-node, ce-enroll, ce-infer-worker)"
fi

exit 0
