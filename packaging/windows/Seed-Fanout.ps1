<#
.SYNOPSIS
  Seed-Fanout.ps1 — Windows counterpart of packaging/linux/seed-fanout.sh.

.DESCRIPTION
  The replicator content path for a Windows SEED node: fan ce / rdev / replicator / ce-infer-worker
  binaries + the model GGUF out across the LAN as an O(log N) attenuating tree, instead of pushing
  from one console to 1500 nodes. A thin wrapper over the cross-platform `replicator.exe` binary
  (verbatim reuse of onward_abilities/attenuate/delegate) — no new logic, ce-fleet just orchestrates.

  Run on a seed node (one per subnet/VLAN, enrolled via SCCM/Intune first) that holds a root-anchored
  cap with `sync,spawn`. `replicator seed` delegates a STRICTLY WEAKER cap to each child (abilities
  intersected, expiry clamped, audience fixed) and drops `spawn` at the last hop so leaves receive but
  do not replicate further. Children come up LAN-only and non-mining (`ce start --no-mine`) — same
  air-gap posture. Makes NO internet call.

  Mirrors the env contract of seed-fanout.sh:
    CE_FLEET_SEED_CAP      (required) the seed root-anchored sync+spawn cap token
    CE_FLEET_FANOUT_DEPTH  default 3
    CE_FLEET_FANOUT_TTL    default 7200 (seconds; clamped to the seed's own cap)
    CE_FLEET_BIN_DIR       default "C:\Program Files\CE"
    CE_FLEET_GGUF          optional local GGUF path to fan out alongside binaries

.EXAMPLE
  $env:CE_FLEET_SEED_CAP = "<token>"
  .\Seed-Fanout.ps1 <target-node-id> [<target-node-id> ...]
#>
[CmdletBinding()]
param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Targets
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Cap = $env:CE_FLEET_SEED_CAP
if ([string]::IsNullOrWhiteSpace($Cap)) {
  throw "set CE_FLEET_SEED_CAP to the seed root-anchored sync+spawn cap token"
}
$Depth  = if ($env:CE_FLEET_FANOUT_DEPTH) { $env:CE_FLEET_FANOUT_DEPTH } else { "3" }
$Ttl    = if ($env:CE_FLEET_FANOUT_TTL)   { $env:CE_FLEET_FANOUT_TTL }   else { "7200" }
$BinDir = if ($env:CE_FLEET_BIN_DIR)      { $env:CE_FLEET_BIN_DIR }      else { "C:\Program Files\CE" }
$Gguf   = $env:CE_FLEET_GGUF

if (-not $Targets -or $Targets.Count -eq 0) {
  Write-Error "usage: `$env:CE_FLEET_SEED_CAP=<token>; .\Seed-Fanout.ps1 <target-node-id> [<target-node-id> ...]"
  exit 2
}

# Children come up LAN-only, non-mining — same air-gap posture as the Linux seed.
$Boot = "ce start --no-mine"

# Build the --bin args for the binaries this seed ships onward (only those present).
$binArgs = @()
foreach ($b in @("ce.exe", "rdev.exe", "replicator.exe", "ce-infer-worker.exe", "ce-infer.exe", "llama-server.exe")) {
  $p = Join-Path $BinDir $b
  if (Test-Path $p) {
    # Name the onward binary without the .exe suffix so the layout matches the Linux fan-out.
    $name = [System.IO.Path]::GetFileNameWithoutExtension($b)
    $binArgs += @("--bin", "$name=$p")
  }
}

# The model GGUF, when provided, rides along (in practice ce-pin/get_object pulls it over the LAN).
if (-not [string]::IsNullOrWhiteSpace($Gguf) -and (Test-Path $Gguf)) {
  $binArgs += @("--bin", "model.gguf=$Gguf")
}

$replicator = Join-Path $BinDir "replicator.exe"
if (-not (Test-Path $replicator)) {
  # Fall back to PATH resolution if the seed staged replicator elsewhere.
  $replicator = "replicator.exe"
}

Write-Host "[seed-fanout] replicating to $($Targets.Count) target(s), depth=$Depth, ttl=${Ttl}s"
$argv = @("seed") + $Targets + @("--cap", $Cap, "--depth", $Depth, "--ttl-secs", $Ttl) + $binArgs + @("--boot", $Boot)
& $replicator @argv
exit $LASTEXITCODE
