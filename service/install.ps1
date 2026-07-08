# LocalLMOS agent installer (Windows). Run in an elevated PowerShell:
#
#   irm https://get.locallmos.os/install.ps1 | iex           # desktop install
# or:
#   .\install.ps1 -SupabaseUrl https://<ref>.supabase.co -AnonKey <ANON> `
#     -Code <PAIRING_CODE> -Name "My Rig"
#
# Downloads a prebuilt, signed agent binary from GitHub Releases, verifies its
# checksum, and installs it. By default this is a desktop install: enroll in the
# signed-in user's config dir when -Code is given, then launch the tray GUI. Pass
# -Service for a headless SYSTEM startup task instead.

# Production locallmos.com backend is baked in as the default (both values are
# public — the anon key ships in the web bundle and is gated by RLS). Override
# with -SupabaseUrl / -AnonKey or the LOCALLMOS_SUPABASE_* env vars.
param(
  [string]$Repo        = $(if ($env:LOCALLMOS_REPO) { $env:LOCALLMOS_REPO } else { "jcam7044/locallmos-agent" }),
  [string]$Channel     = "stable",
  [string]$Version     = "latest",
  [string]$Name        = $env:COMPUTERNAME,
  [string]$Code        = "",
  [string]$SupabaseUrl = $(if ($env:LOCALLMOS_SUPABASE_URL) { $env:LOCALLMOS_SUPABASE_URL } else { "https://fvpjkpfshbvszbcknkqq.supabase.co" }),
  [string]$AnonKey     = $(if ($env:LOCALLMOS_SUPABASE_ANON_KEY) { $env:LOCALLMOS_SUPABASE_ANON_KEY } else { "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImZ2cGprcGZzaGJ2c3piY2tua3FxIiwicm9sZSI6ImFub24iLCJpYXQiOjE3ODI5NzI3MjYsImV4cCI6MjA5ODU0ODcyNn0.b0FDzCAweH6VIwcumLKjNP959unJCUN_egZpb7KdCwg" }),
  [switch]$Service,
  [switch]$Headless,
  [switch]$NoLaunch
)

$ErrorActionPreference = "Stop"
$mode = if ($Service -or $Headless) { "service" } else { "desktop" }

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw "Run this installer from an elevated PowerShell so it can write to $env:ProgramFiles."
}

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
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

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

function Test-EnrolledConfig([string]$ConfigJson) {
  return (Test-Path $ConfigJson) -and (Select-String -Path $ConfigJson -Pattern "refresh_secret" -Quiet)
}

function Get-UserConfigJson {
  $appData = if ($env:APPDATA) { $env:APPDATA } else { [Environment]::GetFolderPath("ApplicationData") }
  return (Join-Path (Join-Path $appData "locallmos-agent") "config.json")
}

# ---- desktop install -------------------------------------------------------
if ($mode -eq "desktop") {
  if (Get-ScheduledTask -TaskName "LocalLMOS Agent" -ErrorAction SilentlyContinue) {
    Write-Host "!! A headless startup task is already installed."
    Write-Host "   Keep it for server mode, or remove it with:"
    Write-Host "   Unregister-ScheduledTask -TaskName 'LocalLMOS Agent' -Confirm:`$false"
  }

  # Desktop mode should use the signed-in user's default config dir, even if a
  # previous service install left LOCALLMOS_CONFIG_DIR in the machine env.
  Remove-Item Env:\LOCALLMOS_CONFIG_DIR -ErrorAction SilentlyContinue
  $env:LOCALLMOS_SUPABASE_URL = $SupabaseUrl
  $env:LOCALLMOS_SUPABASE_ANON_KEY = $AnonKey

  $configJson = Get-UserConfigJson
  if (Test-EnrolledConfig $configJson) {
    Write-Host "==> Already enrolled — skipping enrollment"
  } elseif ($Code -ne "") {
    Write-Host "==> Enrolling desktop app as '$Name'"
    & $binDst enroll --code $Code --name $Name
  } else {
    Write-Host "==> No -Code given. Opening the tray app for local mode and pairing."
  }

  if ($NoLaunch) {
    Write-Host "==> Installed. Launch with: $binDst"
  } else {
    Write-Host "==> Launching LocalLMOS Agent"
    Start-Process -FilePath $binDst
    Start-Sleep -Seconds 2
    Write-Host "==> Done. LocalLMOS Agent is running in your desktop session."
  }
} else {
  New-Item -ItemType Directory -Force -Path $configDir | Out-Null

  # Machine-scoped env so both the service task and manual `enroll` agree.
  [Environment]::SetEnvironmentVariable("LOCALLMOS_CONFIG_DIR", $configDir, "Machine")
  [Environment]::SetEnvironmentVariable("LOCALLMOS_SUPABASE_URL", $SupabaseUrl, "Machine")
  [Environment]::SetEnvironmentVariable("LOCALLMOS_SUPABASE_ANON_KEY", $AnonKey, "Machine")
  $env:LOCALLMOS_CONFIG_DIR = $configDir
  $env:LOCALLMOS_SUPABASE_URL = $SupabaseUrl
  $env:LOCALLMOS_SUPABASE_ANON_KEY = $AnonKey

  # ---- enroll --------------------------------------------------------------
  $configJson = Join-Path $configDir "config.json"
  $serviceReady = $false
  if (Test-EnrolledConfig $configJson) {
    Write-Host "==> Already enrolled — skipping enrollment"
    $serviceReady = $true
  } elseif ($Code -ne "") {
    Write-Host "==> Enrolling as '$Name'"
    & $binDst enroll --code $Code --name $Name
    $serviceReady = $true
  } else {
    Write-Host "!! No -Code given. Generate a pairing code in the dashboard, then run:"
    Write-Host "   `$env:LOCALLMOS_CONFIG_DIR = '$configDir'; & '$binDst' enroll --code <CODE> --name '$Name'"
  }

  if ($serviceReady) {
    # ---- startup task ------------------------------------------------------
    # A relaunch loop supervises the agent: when a self-update exits cleanly,
    # the loop restarts it on the new binary (no reboot needed).
    Write-Host "==> Registering startup task 'LocalLMOS Agent'"
    $loop = "while (`$true) { & '$binDst' service; Start-Sleep -Seconds 5 }"
    $action = New-ScheduledTaskAction -Execute "powershell.exe" `
      -Argument "-NoProfile -WindowStyle Hidden -Command `"$loop`""
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $taskPrincipal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName "LocalLMOS Agent" -Action $action -Trigger $trigger `
      -Principal $taskPrincipal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName "LocalLMOS Agent"

    Write-Host "==> Done. The service task is running and will start at boot."
  } else {
    Write-Host "==> Service files installed but startup task was not created because this rig is not enrolled."
  }
}

# ---- runtime check ---------------------------------------------------------
if (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
  Write-Host ""
  Write-Host "!! Ollama was not detected on this machine."
  Write-Host "   LocalLMOS uses Ollama to run models locally. Install it from:"
  Write-Host "     https://ollama.com/download   (or: winget install Ollama.Ollama)"
  Write-Host "   Then pull a model, e.g.:  ollama pull llama3.2"
}
