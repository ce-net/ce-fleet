#!/bin/sh
# ce-enroll — first-boot fleet enrollment glue (the ce-enroll.service ExecStart).
#
# Flow (zero clinician steps): the node is already up (ce-node.service) and on the LAN mesh. We:
#   1. read the node id from the local CE API,
#   2. probe the inference tier (ce-infer probe),
#   3. POST {node_id, hostname, os, tier, nonce, bootstrap_secret} to the delegate /enroll,
#   4. store the returned audience-bound working cap in the node wallet,
#   5. write <data_dir>/enrolled so this never runs again, and exit 0.
#
# Air-gap: the delegate is a LAN host (CE_FLEET_DELEGATE_URL points at a private/LAN address). This
# script makes NO internet call. POSIX sh + curl only (busybox-compatible).
set -eu

DATA_DIR="${CE_DATA_DIR:-/var/lib/ce}"
NODE_API="${CE_NODE:-http://127.0.0.1:8844}"
DELEGATE="${CE_FLEET_DELEGATE_URL:-}"
SECRET="${CE_FLEET_BOOTSTRAP_SECRET:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --node) NODE_API="$2"; shift 2 ;;
    --delegate) DELEGATE="$2"; shift 2 ;;
    *) shift ;;
  esac
done

ENROLLED_MARKER="$DATA_DIR/enrolled"
if [ -f "$ENROLLED_MARKER" ]; then
  echo "ce-enroll: already enrolled ($ENROLLED_MARKER); nothing to do"
  exit 0
fi

if [ -z "$DELEGATE" ]; then
  echo "ce-enroll: CE_FLEET_DELEGATE_URL not set (no LAN delegate to enroll with) — skipping" >&2
  # Not fatal: a node can mesh and be enrolled later by an admin-issued cap or replicator fan-out.
  exit 0
fi
if [ -z "$SECRET" ]; then
  echo "ce-enroll: CE_FLEET_BOOTSTRAP_SECRET not set — cannot authenticate enrollment" >&2
  exit 1
fi

# 1. node id from the local API (retry: the node may still be coming up in a boot wave).
NODE_ID=""
i=0
while [ "$i" -lt 30 ]; do
  NODE_ID="$(curl -fsS "$NODE_API/status" 2>/dev/null | sed -n 's/.*"node_id"[ ]*:[ ]*"\([0-9a-f]\{64\}\)".*/\1/p' || true)"
  [ -n "$NODE_ID" ] && break
  i=$((i + 1))
  sleep 2
done
if [ -z "$NODE_ID" ]; then
  echo "ce-enroll: could not read node id from $NODE_API/status after retries" >&2
  exit 1
fi

HOSTNAME_VAL="$(hostname 2>/dev/null || echo unknown)"
OS_VAL="linux"

# 2. tier from the inference probe (best-effort; default Ineligible if the worker isn't present).
TIER="Ineligible"
if command -v ce-infer >/dev/null 2>&1; then
  TIER="$(ce-infer probe --quiet 2>/dev/null | sed -n 's/.*tier[ =:]*\([A-Za-z]*\).*/\1/p' | head -n1 || true)"
  [ -z "$TIER" ] && TIER="Ineligible"
fi

# 3. one-time per-boot nonce (urandom; the delegate burns it on first use).
NONCE="$(head -c 16 /dev/urandom 2>/dev/null | od -An -tx1 2>/dev/null | tr -d ' \n' || date +%s%N)"

# An explicit pre-minted enroll token (single-node mode) wins over the shared bootstrap secret.
PRESENTED_SECRET="${CE_ENROLL_TOKEN:-$SECRET}"

PAYLOAD="$(printf '{"node_id":"%s","hostname":"%s","os":"%s","tier":"%s","nonce":"%s","bootstrap_secret":"%s"}' \
  "$NODE_ID" "$HOSTNAME_VAL" "$OS_VAL" "$TIER" "$NONCE" "$PRESENTED_SECRET")"

echo "ce-enroll: enrolling node ${NODE_ID%????????????????????????????????????????????????} (tier=$TIER) at $DELEGATE"
RESP="$(curl -fsS -X POST "$DELEGATE/enroll" -H 'Content-Type: application/json' -d "$PAYLOAD" || true)"
if [ -z "$RESP" ]; then
  echo "ce-enroll: delegate /enroll returned no response (will retry on next boot)" >&2
  exit 1
fi

WORKING_CAP="$(printf '%s' "$RESP" | sed -n 's/.*"working_cap"[ ]*:[ ]*"\([0-9a-f]*\)".*/\1/p')"
if [ -z "$WORKING_CAP" ]; then
  echo "ce-enroll: enrollment rejected: $RESP" >&2
  exit 1
fi

# 4. store the working cap in the node wallet for the delegate that issued it.
mkdir -p "$DATA_DIR/wallet"
printf '%s' "$WORKING_CAP" > "$DATA_DIR/wallet/fleet-working.cap"
chmod 600 "$DATA_DIR/wallet/fleet-working.cap" 2>/dev/null || true
# Best-effort registration with the local node's wallet (ignore if the CLI shape differs).
if command -v ce >/dev/null 2>&1; then
  ce wallet add fleet "$NODE_ID" --cap "$WORKING_CAP" >/dev/null 2>&1 || true
fi

# 5. mark enrolled — the worker can now start.
printf 'node_id=%s\ntier=%s\nenrolled_at=%s\n' "$NODE_ID" "$TIER" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$ENROLLED_MARKER"
echo "ce-enroll: enrolled; working cap stored, $ENROLLED_MARKER written"
exit 0
