<#
.SYNOPSIS
  ce-enroll (Windows) — first-boot fleet enrollment, the scheduled-task counterpart of ce-enroll.sh.

.DESCRIPTION
  Runs at startup until the machine is enrolled. Reads the node id from the local CE API, probes the
  inference tier, POSTs {node_id, hostname, os, tier, nonce, bootstrap_secret} to the LAN delegate
  /enroll, stores the returned audience-bound working cap in the node wallet, writes
  <DataDir>\enrolled, and exits. Zero clinician steps. Makes NO internet call (delegate is a LAN host).
#>
[CmdletBinding()]
param(
  [string]$DataDir = "C:\ProgramData\ce",
  [string]$NodeApi = "http://127.0.0.1:8844"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$enrolledMarker = Join-Path $DataDir "enrolled"
if (Test-Path $enrolledMarker) { Write-Host "[ce-enroll] already enrolled"; exit 0 }

# Load enroll env (delegate URL + bootstrap secret).
$envFile = Join-Path $DataDir "enroll.env"
$envMap = @{}
if (Test-Path $envFile) {
  foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*([^#=]+)=(.*)$') { $envMap[$matches[1].Trim()] = $matches[2].Trim() }
  }
}
$delegate = $envMap["CE_FLEET_DELEGATE_URL"]
$secret   = $envMap["CE_FLEET_BOOTSTRAP_SECRET"]
$token    = $envMap["CE_ENROLL_TOKEN"]
$presented = if (![string]::IsNullOrWhiteSpace($token)) { $token } else { $secret }

if ([string]::IsNullOrWhiteSpace($delegate)) {
  Write-Host "[ce-enroll] no delegate URL configured; node will mesh and can be enrolled later. Skipping."
  exit 0
}
if ([string]::IsNullOrWhiteSpace($presented)) {
  Write-Error "[ce-enroll] no bootstrap secret/token; cannot authenticate enrollment"
  exit 1
}

# 1. node id (retry during a boot wave).
$nodeId = $null
for ($i = 0; $i -lt 30 -and -not $nodeId; $i++) {
  try {
    $status = Invoke-RestMethod -Uri "$NodeApi/status" -TimeoutSec 5
    $nodeId = $status.node_id
  } catch { Start-Sleep -Seconds 2 }
}
if (-not $nodeId) { Write-Error "[ce-enroll] could not read node id from $NodeApi/status"; exit 1 }

# 2. tier from the probe (best-effort).
$tier = "Ineligible"
$inferExe = "C:\Program Files\CE\ce-infer.exe"
if (Test-Path $inferExe) {
  try {
    $out = & $inferExe probe --quiet 2>$null
    if ($out -match 'tier[ =:]*([A-Za-z]+)') { $tier = $matches[1] }
  } catch {}
}

# 3. one-time per-boot nonce.
$nonce = [guid]::NewGuid().ToString("N")

$body = @{
  node_id          = $nodeId
  hostname         = [System.Net.Dns]::GetHostName()
  os               = "windows"
  tier             = $tier
  nonce            = $nonce
  bootstrap_secret = $presented
} | ConvertTo-Json -Compress

Write-Host "[ce-enroll] enrolling node (tier=$tier) at $delegate"
try {
  $resp = Invoke-RestMethod -Uri "$delegate/enroll" -Method Post -ContentType "application/json" -Body $body -TimeoutSec 30
} catch {
  Write-Error "[ce-enroll] delegate /enroll failed: $_"
  exit 1
}

if ([string]::IsNullOrWhiteSpace($resp.working_cap)) {
  Write-Error "[ce-enroll] enrollment rejected: $($resp | ConvertTo-Json -Compress)"
  exit 1
}

# 4. store the working cap.
$walletDir = Join-Path $DataDir "wallet"
New-Item -ItemType Directory -Force -Path $walletDir | Out-Null
$capPath = Join-Path $walletDir "fleet-working.cap"
Set-Content -Path $capPath -Value $resp.working_cap -NoNewline -Encoding ascii
icacls $capPath /inheritance:r /grant:r "SYSTEM:F" "Administrators:F" | Out-Null

$ceExe = "C:\Program Files\CE\ce.exe"
if (Test-Path $ceExe) { & $ceExe wallet add fleet $nodeId --cap $resp.working_cap 2>$null | Out-Null }

# 5. mark enrolled.
"node_id=$nodeId`r`ntier=$tier`r`nenrolled_at=$([DateTime]::UtcNow.ToString('o'))" | Set-Content -Path $enrolledMarker -Encoding ascii
Write-Host "[ce-enroll] enrolled; working cap stored, marker written"

# Once enrolled, drop the startup task so it stops re-running.
Unregister-ScheduledTask -TaskName "ce-enroll" -Confirm:$false -ErrorAction SilentlyContinue
exit 0
