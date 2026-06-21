#!/bin/sh
# seed-fanout.sh — the replicator content path: fan ce/rdev/replicator/ce-infer-worker binaries +
# the model GGUF out across the LAN as an O(log N) attenuating tree, instead of pushing from one
# console to 1500 nodes.
#
# Run this on a SEED node (one per subnet/VLAN, ~30-50 total, enrolled via SCCM/Ansible first). The
# seed holds a root-anchored cap with `sync,spawn`; `replicator seed` delegates a STRICTLY WEAKER
# cap to each child (abilities intersected, expiry clamped, audience fixed), and at the last hop
# drops `spawn` so leaves can receive but not replicate further. Binary/model updates reach every
# node in 2-3 LAN hops. SCCM/Ansible remains the audited install-of-record; this is the fast path.
#
# This is a thin wrapper over the existing `replicator` binary (replicator/src/main.rs) — verbatim
# reuse of onward_abilities/attenuate/delegate. No new code; ce-fleet just orchestrates it.
set -eu

CAP="${CE_FLEET_SEED_CAP:?set CE_FLEET_SEED_CAP to the seed root-anchored sync+spawn cap token}"
DEPTH="${CE_FLEET_FANOUT_DEPTH:-3}"     # 3 hops covers a large subnet at O(log N)
TTL="${CE_FLEET_FANOUT_TTL:-7200}"       # 2h delegated-cap lifetime (clamped to the seed's own)
BIN_DIR="${CE_FLEET_BIN_DIR:-/usr/local/bin}"
GGUF="${CE_FLEET_GGUF:-}"                 # optional: a local GGUF path to fan out alongside binaries

if [ "$#" -eq 0 ]; then
  echo "usage: CE_FLEET_SEED_CAP=<token> $0 <target-node-id> [<target-node-id> ...]" >&2
  exit 2
fi

set -- "$@"
BOOT='ce start --no-mine'   # children come up LAN-only, non-mining — same air-gap posture

# Build the --bin args for the binaries this seed ships onward.
BIN_ARGS=""
for b in ce rdev replicator ce-infer-worker ce-infer llama-server; do
  if [ -x "$BIN_DIR/$b" ]; then
    BIN_ARGS="$BIN_ARGS --bin $b=$BIN_DIR/$b"
  fi
done

# The model GGUF, when provided, rides along (in practice ce-pin/get_object pulls it over the LAN;
# this --bin path is for the seed-to-child push when a child has no peer holding the CID yet).
if [ -n "$GGUF" ] && [ -f "$GGUF" ]; then
  BIN_ARGS="$BIN_ARGS --bin model.gguf=$GGUF"
fi

echo "seed-fanout: replicating to $# target(s), depth=$DEPTH, ttl=${TTL}s"
# shellcheck disable=SC2086
exec replicator seed "$@" \
  --cap "$CAP" \
  --depth "$DEPTH" \
  --ttl-secs "$TTL" \
  $BIN_ARGS \
  --boot "$BOOT"
