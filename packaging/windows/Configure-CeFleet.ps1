<#
.SYNOPSIS
  Configure-CeFleet — the MSI deferred custom action: pin the org root pubkey, write enroll.env,
  register the first-boot ce-enroll startup task, and apply the air-gap egress firewall.

.DESCRIPTION
  Invoked by CeFleet.wxs after InstallFiles (deferred, runs as SYSTEM). Separated from
  Install-CeFleet.ps1 (the standalone script path) so the MSI can reuse the same configuration
  logic without re-copying binaries (the MSI already placed them).
#>
[CmdletBinding()]
param(
  [string]$RootKeyHex = "",
  [string]$DelegateUrl = "",
  [string]$BootstrapSecret = "",
  [string]$EnrollToken = "",
  [string]$DataDir = "C:\ProgramData\ce"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$InstallDir = "C:\Program Files\CE"
$RootsDir = Join-Path $DataDir "roots"
New-Item -ItemType Directory -Force -Path $DataDir, $RootsDir, (Join-Path $DataDir "wallet") | Out-Null

# 1. pin org PUBLIC root key.
if (-not [string]::IsNullOrWhiteSpace($RootKeyHex)) {
  $rootKeyPath = Join-Path $RootsDir "ce-root.pub"
  Set-Content -Path $rootKeyPath -Value $RootKeyHex.Trim() -NoNewline -Encoding ascii
}

# 2. enroll.env.
$enrollEnv = Join-Path $DataDir "enroll.env"
@(
  "CE_FLEET_DELEGATE_URL=$DelegateUrl",
  "CE_FLEET_BOOTSTRAP_SECRET=$BootstrapSecret",
  "CE_ENROLL_TOKEN=$EnrollToken",
  "CE_DATA_DIR=$DataDir"
) | Set-Content -Path $enrollEnv -Encoding ascii
icacls $enrollEnv /inheritance:r /grant:r "SYSTEM:F" "Administrators:F" | Out-Null

# 3. first-boot enroll task.
$enrollPs1 = Join-Path $InstallDir "Invoke-CeEnroll.ps1"
if (Test-Path $enrollPs1) {
  $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$enrollPs1`" -DataDir `"$DataDir`""
  $trigger = New-ScheduledTaskTrigger -AtStartup
  $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
  Register-ScheduledTask -TaskName "ce-enroll" -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null
}

# 4. air-gap egress: allow LAN/loopback, block internet outbound for the CE binaries.
foreach ($bin in @("ce.exe", "ce-infer-worker.exe", "rdev.exe", "replicator.exe", "llama-server.exe")) {
  $p = Join-Path $InstallDir $bin
  if (Test-Path $p) {
    New-NetFirewallRule -DisplayName "CE allow LAN out ($bin)" -Direction Outbound -Program $p -RemoteAddress @("LocalSubnet","10.0.0.0/8","172.16.0.0/12","192.168.0.0/16","169.254.0.0/16","127.0.0.0/8") -Action Allow -ErrorAction SilentlyContinue | Out-Null
    New-NetFirewallRule -DisplayName "CE block internet out ($bin)" -Direction Outbound -Program $p -RemoteAddress Internet -Action Block -ErrorAction SilentlyContinue | Out-Null
  }
}

exit 0
