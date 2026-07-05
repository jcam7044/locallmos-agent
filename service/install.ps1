# LocalLMOS agent installer (Windows). Run in an elevated PowerShell:
#
#   irm https://get.locallmos.os/install.ps1 | iex           # then follow prompts
# or:
#   .\install.ps1 -SupabaseUrl https://<ref>.supabase.co -AnonKey <ANON> `
#     -Code <PAIRING_CODE> -Name "My Rig"
#
# Downloads a prebuilt, signed agent binary from GitHub Releases, verifies its
# checksum, installs it, registers a startup task, and enrolls the rig. The
# startup task runs the agent in a relaunch loop so a self-update (which exits
# cleanly) comes back up on the new binary without a reboot.

param(
  [string]$Repo        = $(if ($env:LOCALLMOS_REPO) { $env:LOCALLMOS_REPO } else { "jcam7044/locallmos" }),
  [string]$Channel     = "stable",
  [string]$Version     = "latest",
  [string]$Name        = $env:COMPUTERNAME,
  [string]$Code        = "",
  [string]$SupabaseUrl = $env:LOCALLMOS_SUPABASE_URL,
  [string]$AnonKey     = $env:LOCALLMOS_SUPABASE_ANON_KEY
)

$ErrorActionPreference = "Stop"

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
# CI only publishes a windows-x86_64 build today; fail clearly on ARM64 rather
# than 404 on download. Keep in sync with the release.yml build matrix.
if ($arch -ne "x86_64") {
  throw "No prebuilt agent for Windows $arch yet (only x86_64 is published)."
}
$asset = "locallmos-agent-windows-$arch.exe"
$base = if ($Version -eq "latest") {
  "https://github.com/$Repo/releases/latest/download"
} else {
  "https://github.com/$Repo/releases/download/$Version"
}

$installDir = Join-Path $env:ProgramFiles "LocalLMOS"
$binDst = Join-Path $installDir "locallmos-agent.exe"
$configDir = Join-Path $env:ProgramData "locallmos-agent"
New-Item -ItemType Directory -Force -Path $installDir, $configDir | Out-Null

$tmp = Join-Path $env:TEMP "locallmos-agent.exe"
$tmpSha = "$tmp.sha256"

Write-Host "==> Downloading $asset ($Version)"
Invoke-WebRequest -Uri "$base/$asset" -OutFile $tmp
Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $tmpSha

Write-Host "==> Verifying checksum"
$expected = (Get-Content $tmpSha).Split(" ")[0].Trim().ToLower()
$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
if ($expected -ne $actual) {
  throw "checksum mismatch: expected $expected, got $actual"
}

Write-Host "==> Installing to $binDst"
Copy-Item $tmp $binDst -Force

# Machine-scoped env so both the service task and manual `enroll` agree.
[Environment]::SetEnvironmentVariable("LOCALLMOS_CONFIG_DIR", $configDir, "Machine")
[Environment]::SetEnvironmentVariable("LOCALLMOS_SUPABASE_URL", $SupabaseUrl, "Machine")
[Environment]::SetEnvironmentVariable("LOCALLMOS_SUPABASE_ANON_KEY", $AnonKey, "Machine")
$env:LOCALLMOS_CONFIG_DIR = $configDir
$env:LOCALLMOS_SUPABASE_URL = $SupabaseUrl
$env:LOCALLMOS_SUPABASE_ANON_KEY = $AnonKey

# ---- enroll ----------------------------------------------------------------
$configJson = Join-Path $configDir "config.json"
if ((Test-Path $configJson) -and (Select-String -Path $configJson -Pattern "refresh_secret" -Quiet)) {
  Write-Host "==> Already enrolled — skipping enrollment"
} elseif ($Code -ne "") {
  Write-Host "==> Enrolling as '$Name'"
  & $binDst enroll --code $Code --name $Name
} else {
  Write-Host "!! No -Code given. Generate a pairing code in the dashboard, then run:"
  Write-Host "   & '$binDst' enroll --code <CODE> --name '$Name'"
}

# ---- startup task ----------------------------------------------------------
# A relaunch loop supervises the agent: when a self-update exits cleanly, the
# loop restarts it on the new binary (no reboot needed).
Write-Host "==> Registering startup task 'LocalLMOS Agent'"
$loop = "while (`$true) { & '$binDst' service; Start-Sleep -Seconds 5 }"
$action = New-ScheduledTaskAction -Execute "powershell.exe" `
  -Argument "-NoProfile -WindowStyle Hidden -Command `"$loop`""
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName "LocalLMOS Agent" -Action $action -Trigger $trigger `
  -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName "LocalLMOS Agent"

Write-Host "==> Done. The agent is running and will start at boot."

# ---- runtime check ---------------------------------------------------------
if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
  Write-Host ""
  Write-Host "!! Ollama was not detected on this machine."
  Write-Host "   LocalLMOS uses Ollama to run models locally. Install it from:"
  Write-Host "     https://ollama.com/download   (or: winget install Ollama.Ollama)"
  Write-Host "   Then pull a model, e.g.:  ollama pull llama3.2"
}
