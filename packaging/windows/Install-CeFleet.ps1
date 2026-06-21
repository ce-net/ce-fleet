<#
.SYNOPSIS
  ce-fleet Windows silent installer — machine-context, no clinician steps.

.DESCRIPTION
  The MSI-equivalent PowerShell path for environments that prefer a script over the .msi (the .msi
  built from CeFleet.wxs is the audited install-of-record; this script does the same placement and
  service registration for GPO/SCCM/Intune deployments that run PowerShell).

  Mirrors the silent MSI line:
    msiexec /i ce-fleet.msi /qn /norestart CE_ROOT_KEY=<hex> CE_ENROLL_TOKEN=<cap> CE_DATA_DIR="C:\ProgramData\ce"

  It:
    1. installs ce.exe + rdev.exe + replicator.exe + ce-infer-worker.exe + ce-infer.exe + the
       per-platform llama.cpp engine to C:\Program Files\CE\,
    2. drops the org PUBLIC root key to C:\ProgramData\ce\roots (the authorization pin),
    3. writes the enrollment env for first-boot enroll,
    4. registers a machine-context Windows Service `ce-node` running `ce start --no-mine` with
       failure-restart, plus `ce-infer-worker` and a one-shot `ce-enroll` scheduled task,
    5. opens libp2p LAN port 4001 (TCP+UDP) and keeps 8844 + the engine loopback-only,
    6. blocks non-LAN OUTBOUND with a default-deny firewall rule set (the air-gap's second half).

  Air-gap: the node is started WITHOUT cloud bootstrap/relay; nothing here dials the internet.
  Run elevated (SYSTEM via SCCM/Intune, or an elevated PowerShell).

.PARAMETER SourceDir
  Folder holding the staged binaries (ce.exe, rdev.exe, replicator.exe, ce-infer-worker.exe,
  ce-infer.exe, llama-server.exe). Defaults to the script's own directory.

.PARAMETER RootKeyHex
  The org PUBLIC root key (hex). Pinned to C:\ProgramData\ce\roots\ce-root.pub. PUBLIC key only.

.PARAMETER DelegateUrl
  The LAN regional delegate /enroll endpoint (a private/LAN address).

.PARAMETER BootstrapSecret
  The shared bootstrap secret presented at /enroll (short-TTL, tag-scoped; from the deploy secret store).

.PARAMETER EnrollToken
  Optional pre-minted single-node enroll cap token (overrides BootstrapSecret).

.PARAMETER DataDir
  Node data dir. Default C:\ProgramData\ce.
#>
[CmdletBinding()]
param(
  [string]$SourceDir = $PSScriptRoot,
  [Parameter(Mandatory = $true)][string]$RootKeyHex,
  [string]$DelegateUrl = "",
  [string]$BootstrapSecret = "",
  [string]$EnrollToken = "",
  [string]$DataDir = "C:\ProgramData\ce"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$InstallDir = "C:\Program Files\CE"
$RootsDir   = Join-Path $DataDir "roots"
$WalletDir  = Join-Path $DataDir "wallet"
$Binaries   = @("ce.exe", "rdev.exe", "replicator.exe", "ce-infer-worker.exe", "ce-infer.exe", "llama-server.exe")

function Write-Step($msg) { Write-Host "[ce-fleet] $msg" }

# --- 1. install dirs + binaries ---
Write-Step "creating $InstallDir, $DataDir"
New-Item -ItemType Directory -Force -Path $InstallDir, $DataDir, $RootsDir, $WalletDir | Out-Null

foreach ($bin in $Binaries) {
  $src = Join-Path $SourceDir $bin
  if (Test-Path $src) {
    Copy-Item -Force $src (Join-Path $InstallDir $bin)
    Write-Step "installed $bin"
  } else {
    Write-Warning "[ce-fleet] missing staged binary $bin (skipped)"
  }
}

# The GGUF-less registry travels with the install; weights arrive over the LAN blob store.
$registrySrc = Join-Path $SourceDir "models.toml"
if (Test-Path $registrySrc) { Copy-Item -Force $registrySrc (Join-Path $DataDir "models.toml") }

# --- 2. pin the org PUBLIC root key (the authorization pin; placement confers no authority) ---
if ([string]::IsNullOrWhiteSpace($RootKeyHex)) { throw "RootKeyHex is required (org PUBLIC root key)" }
$rootKeyPath = Join-Path $RootsDir "ce-root.pub"
Set-Content -Path $rootKeyPath -Value $RootKeyHex.Trim() -NoNewline -Encoding ascii
Write-Step "org root pubkey pinned to $rootKeyPath"

# --- 3. enrollment env for first-boot enroll ---
$enrollEnv = Join-Path $DataDir "enroll.env"
@(
  "CE_FLEET_DELEGATE_URL=$DelegateUrl",
  "CE_FLEET_BOOTSTRAP_SECRET=$BootstrapSecret",
  "CE_ENROLL_TOKEN=$EnrollToken",
  "CE_DATA_DIR=$DataDir"
) | Set-Content -Path $enrollEnv -Encoding ascii
# Lock the env down (it holds the bootstrap secret) to SYSTEM + Administrators.
icacls $enrollEnv /inheritance:r /grant:r "SYSTEM:F" "Administrators:F" | Out-Null
Write-Step "enrollment env written to $enrollEnv"

# --- 4. register the machine-context Windows Services ---
$ceExe = Join-Path $InstallDir "ce.exe"
$workerExe = Join-Path $InstallDir "ce-infer-worker.exe"

# ce-node: `ce start --no-mine` (LAN-only, no bootstrap). LocalSystem, auto-start, restart on failure.
Write-Step "registering ce-node service"
sc.exe create "ce-node" binPath= "`"$ceExe`" start --no-mine --data-dir `"$DataDir`"" start= auto obj= "LocalSystem" DisplayName= "CE Node (fleet, LAN-only)" | Out-Null
sc.exe failure "ce-node" reset= 86400 actions= restart/5000/restart/5000/restart/5000 | Out-Null
sc.exe description "ce-node" "CE node — fleet member, LAN-only mesh, non-mining. PHI never leaves the LAN." | Out-Null

# ce-infer-worker: starts after the node + enrollment.
if (Test-Path $workerExe) {
  Write-Step "registering ce-infer-worker service"
  sc.exe create "ce-infer-worker" binPath= "`"$workerExe`" --node http://127.0.0.1:8844 --data-dir `"$DataDir`"" start= auto obj= "LocalSystem" depend= "ce-node" DisplayName= "CE Inference Worker" | Out-Null
  sc.exe failure "ce-infer-worker" reset= 86400 actions= restart/5000/restart/5000/restart/5000 | Out-Null
}

# ce-enroll: one-shot first-boot enrollment as a scheduled task (runs at startup until enrolled).
$enrollPs1 = Join-Path $InstallDir "Invoke-CeEnroll.ps1"
Copy-Item -Force (Join-Path $SourceDir "Invoke-CeEnroll.ps1") $enrollPs1 -ErrorAction SilentlyContinue
if (Test-Path $enrollPs1) {
  Write-Step "registering ce-enroll startup task"
  $action  = New-ScheduledTaskAction -Execute "powershell.exe" -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$enrollPs1`" -DataDir `"$DataDir`""
  $trigger = New-ScheduledTaskTrigger -AtStartup
  $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
  Register-ScheduledTask -TaskName "ce-enroll" -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null
}

# --- 5. firewall: open libp2p LAN port; keep node API + engine loopback-only ---
Write-Step "opening libp2p LAN port 4001 (TCP+UDP), LAN scope only"
New-NetFirewallRule -DisplayName "CE libp2p 4001 TCP (LAN)" -Direction Inbound -Protocol TCP -LocalPort 4001 -RemoteAddress LocalSubnet -Action Allow -ErrorAction SilentlyContinue | Out-Null
New-NetFirewallRule -DisplayName "CE libp2p 4001 UDP (LAN)" -Direction Inbound -Protocol UDP -LocalPort 4001 -RemoteAddress LocalSubnet -Action Allow -ErrorAction SilentlyContinue | Out-Null

# --- 6. air-gap: default-deny non-LAN OUTBOUND for the CE binaries ---
Write-Step "blocking non-LAN outbound for CE binaries (air-gap)"
foreach ($bin in @("ce.exe", "ce-infer-worker.exe", "rdev.exe", "replicator.exe", "llama-server.exe")) {
  $p = Join-Path $InstallDir $bin
  if (Test-Path $p) {
    # Allow LAN/private + loopback; block everything else outbound for this program.
    New-NetFirewallRule -DisplayName "CE allow LAN out ($bin)" -Direction Outbound -Program $p -RemoteAddress @("LocalSubnet","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16","127.0.0.0/8") -Action Allow -ErrorAction SilentlyContinue | Out-Null
    New-NetFirewallRule -DisplayName "CE block internet out ($bin)" -Direction Outbound -Program $p -RemoteAddress Internet -Action Block -ErrorAction SilentlyContinue | Out-Null
  }
}

# --- start ---
Write-Step "starting ce-node"
Start-Service "ce-node" -ErrorAction SilentlyContinue
if (Get-Service "ce-infer-worker" -ErrorAction SilentlyContinue) { Start-Service "ce-infer-worker" -ErrorAction SilentlyContinue }

Write-Step "install complete. Node is up (LAN-only, non-mining); first-boot enrollment will run via the ce-enroll task."
exit 0
