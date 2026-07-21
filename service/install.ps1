# LocalLMOS agent installer (Windows). Run in an elevated PowerShell:
#
#   iex ((curl.exe -fsSL https://locallmos.com/install.ps1) -join "`n")
# or:
#   .\install.ps1 -SupabaseUrl https://<ref>.supabase.co -AnonKey <ANON> `
#     -Code <PAIRING_CODE> -Name "My Rig"
#
# Downloads a prebuilt, signed agent binary from GitHub Releases, verifies its
# checksum, and installs it. By default this is a desktop install: enroll in the
# signed-in user's config dir when -Code is given, then launch the tray GUI. Pass
# -Service for a headless SYSTEM startup task instead.
#
# Pass -Runtime llamacpp to provision a hardware-appropriate llama-server
# (cuda/hip/vulkan/cpu, auto-detected) instead of using Ollama; -LlamaCppBackend
# forces a specific one (no fallback).

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
  # Local LLM engine. "ollama" (default) leaves current installs unchanged;
  # "llamacpp" provisions a hardware-appropriate llama-server. Windows CUDA/HIP/
  # Vulkan/CPU prebuilts all come from upstream ggml-org/llama.cpp (unlike Linux,
  # upstream ships a Windows CUDA build), so LlamaCppRepo defaults there.
  [string]$Runtime         = $(if ($env:LOCALLMOS_RUNTIME) { $env:LOCALLMOS_RUNTIME } else { "ollama" }),
  [string]$LlamaCppRepo    = $(if ($env:LOCALLMOS_LLAMACPP_REPO) { $env:LOCALLMOS_LLAMACPP_REPO } else { "ggml-org/llama.cpp" }),
  [string]$LlamaCppVersion = $(if ($env:LOCALLMOS_LLAMACPP_VERSION) { $env:LOCALLMOS_LLAMACPP_VERSION } else { "b10068" }),
  [string]$LlamaCppBackend = $(if ($env:LOCALLMOS_LLAMACPP_BACKEND) { $env:LOCALLMOS_LLAMACPP_BACKEND } else { "auto" }),
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

if ($Runtime -notin @("ollama", "llamacpp")) {
  throw "unknown runtime: $Runtime (expected ollama or llamacpp)"
}
if ($LlamaCppBackend -notin @("auto", "cuda", "hip", "vulkan", "cpu")) {
  throw "unknown llamacpp backend: $LlamaCppBackend (expected auto|cuda|hip|vulkan|cpu)"
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

# ---- llama.cpp provisioning ------------------------------------------------
# Windows counterpart of service/lib-llamacpp.sh: detect the best backend, then
# download + smoke-test-fallback through the chain. Windows CUDA is a prebuilt
# from upstream (with the companion cudart zip), so no on-device build anywhere.

function Resolve-LlamaCppTag([string]$Version, [string]$Repo) {
  if ($Version -ne "latest") { return $Version }
  try {
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
      -Headers @{ "User-Agent" = "locallmos-installer" }
    return $rel.tag_name
  } catch { return "" }
}

# Echo the hosted CUDA variant this rig's NVIDIA driver can load (mirrors the
# Linux _llx_cuda_variant / the 12.4-13.3 split): >= 13.0 -> 13.3, >= 12.4 -> 12.4,
# else "" (too old / no NVIDIA -> not cuda).
function Get-CudaVariant {
  if (-not (Get-Command nvidia-smi -ErrorAction SilentlyContinue)) { return "" }
  $out = (& nvidia-smi 2>&1 | Out-String)
  if ($out -match 'CUDA Version:\s*(\d+)\.(\d+)') {
    $maj = [int]$Matches[1]; $min = [int]$Matches[2]
    if ($maj -ge 13) { return "13.3" }
    if ($maj -eq 12 -and $min -ge 4) { return "12.4" }
  }
  return ""
}

# iGPU policy (parallels the Linux qualifying-GPU heuristic). AdapterRAM is a
# uint32 that caps at 4 GB and is unreliable, so qualification is name-based: a
# controller qualifies unless it's a known integrated part, or if it's on the
# unified-memory whitelist (Strix Halo).
function Test-IntegratedGpu([string]$Name) {
  return ($Name -match 'Radeon(\(TM\))? Graphics$') -or ($Name -match 'Intel.*(UHD|Iris|HD Graphics)')
}
function Test-QualifyingController([string]$Name) {
  if ($Name -match 'Radeon 8050S|Radeon 8060S') { return $true }   # Strix Halo APU
  return (-not (Test-IntegratedGpu $Name))
}

# Detect the best backend: cuda -> hip -> vulkan -> cpu, applying the iGPU policy.
function Get-LlamaCppBackend {
  if (Get-CudaVariant) { return "cuda" }
  $names = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
  $amdQualifies = $false
  foreach ($n in $names) {
    if (($n -match 'AMD|Radeon') -and (Test-QualifyingController $n)) { $amdQualifies = $true }
  }
  $hipDll = Test-Path (Join-Path $env:SystemRoot "System32\amdhip64*.dll")
  if ($amdQualifies -and $hipDll) { return "hip" }
  foreach ($n in $names) { if (Test-QualifyingController $n) { return "vulkan" } }
  return "cpu"
}

function Get-LlamaCppChain([string]$Backend) {
  switch ($Backend) {
    "cuda"   { return @("cuda", "vulkan", "cpu") }
    "hip"    { return @("hip", "vulkan", "cpu") }
    "vulkan" { return @("vulkan", "cpu") }
    default  { return @("cpu") }
  }
}

# Primary asset zip for a backend; "" when unavailable on this rig.
function Get-LlamaCppAsset([string]$Backend, [string]$Tag) {
  switch ($Backend) {
    "cuda"   { $v = Get-CudaVariant; if ($v) { return "llama-$Tag-bin-win-cuda-$v-x64.zip" } else { return "" } }
    "hip"    { return "llama-$Tag-bin-win-hip-radeon-x64.zip" }
    "vulkan" { return "llama-$Tag-bin-win-vulkan-x64.zip" }
    "cpu"    { return "llama-$Tag-bin-win-cpu-x64.zip" }
    default  { return "" }
  }
}

# Companion CUDA runtime zip (unpacked alongside the cuda build). Note: its name
# carries no tag. "" for non-cuda backends.
function Get-CudartAsset([string]$Backend) {
  if ($Backend -eq "cuda") {
    $v = Get-CudaVariant
    if ($v) { return "cudart-llama-bin-win-cuda-$v-x64.zip" }
  }
  return ""
}

function Find-LlamaServer([string]$Root) {
  if (-not (Test-Path $Root)) { return $null }
  $f = Get-ChildItem -Path $Root -Filter "llama-server.exe" -Recurse -ErrorAction SilentlyContinue |
    Select-Object -First 1
  if ($f) { return $f.FullName }
  return $null
}

function Test-LlamaServer([string]$Bin) {
  try {
    $null = & $Bin --version 2>&1
    return ($LASTEXITCODE -eq 0)
  } catch { return $false }
}

function Get-MarkerValue([string]$Marker, [string]$Key) {
  if (-not (Test-Path $Marker)) { return "" }
  foreach ($line in (Get-Content $Marker)) {
    if ($line -match "^$Key=(.*)$") { return $Matches[1].Trim() }
  }
  return ""
}

function Install-LlamaCpp([string]$Backend, [string]$Tag, [string]$Repo, [string]$Mode) {
  $llamaDir = Join-Path (Join-Path $env:ProgramFiles "LocalLMOS") "llama"
  $modelsDir = if ($Mode -eq "service") {
    Join-Path $env:ProgramData "locallmos\models"
  } else {
    Join-Path $env:APPDATA "locallmos\models"
  }
  $marker = Join-Path $llamaDir ".locallmos-llamacpp"

  $forced = ($Backend -ne "auto")
  $target = if ($forced) { $Backend } else { Get-LlamaCppBackend }
  $chain = if ($forced) { @($Backend) } else { Get-LlamaCppChain $target }
  Write-Host "==> Provisioning llama.cpp: tag=$Tag mode=$Backend target=$target (windows-x86_64)"

  # Idempotency: reuse iff the marker records the same backend + tag.
  $existing = Find-LlamaServer $llamaDir
  if ($existing -and (Test-Path $marker)) {
    $mb = Get-MarkerValue $marker "backend"
    $mt = Get-MarkerValue $marker "tag"
    if ($mb -eq $target -and $mt -eq $Tag) {
      Write-Host "==> llama-server already provisioned (backend=$mb tag=$mt)"
      New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
      return @{ Bin = $existing; Backend = $mb; ModelsDir = $modelsDir }
    }
  }

  $committed = $null
  foreach ($b in $chain) {
    Write-Host "==> staging backend: $b"
    $asset = Get-LlamaCppAsset $b $Tag
    if (-not $asset) {
      Write-Host "   no $b asset for windows-x64 at $Tag"
      if ($forced) { throw "forced backend '$b' has no asset; drop -LlamaCppBackend for auto fallback" }
      continue
    }
    $stage = Join-Path $env:TEMP ("llx-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $ok = $true
    try {
      $zip = Join-Path $stage "main.zip"
      Invoke-WebRequest -Uri "https://github.com/$Repo/releases/download/$Tag/$asset" -OutFile $zip
      Expand-Archive -Path $zip -DestinationPath $stage -Force
      Remove-Item $zip -Force
      $cudart = Get-CudartAsset $b
      if ($cudart) {
        $czip = Join-Path $stage "cudart.zip"
        Invoke-WebRequest -Uri "https://github.com/$Repo/releases/download/$Tag/$cudart" -OutFile $czip
        Expand-Archive -Path $czip -DestinationPath $stage -Force
        Remove-Item $czip -Force
      }
    } catch {
      Write-Host "   download/extract failed for ${b}: $($_.Exception.Message)"
      $ok = $false
    }
    $bin = if ($ok) { Find-LlamaServer $stage } else { $null }
    if (-not ($bin -and (Test-LlamaServer $bin))) {
      if ($bin) { Write-Host "   smoke test failed for '$b'" }
      Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
      if ($forced) { throw "forced backend '$b' could not be provisioned; drop -LlamaCppBackend for auto fallback" }
      continue
    }
    Write-Host "==> installing backend '$b' to $llamaDir"
    if (Test-Path $llamaDir) { Remove-Item $llamaDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $llamaDir | Out-Null
    Copy-Item (Join-Path $stage "*") $llamaDir -Recurse -Force
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
    Set-Content -Path $marker -Value @("backend=$b", "tag=$Tag") -Encoding ASCII
    $committed = $b
    break
  }

  if (-not $committed) {
    if ($existing) {
      Write-Host "!! no backend in the chain could be provisioned; keeping the existing install ($existing)"
      New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
      return @{ Bin = $existing; Backend = (Get-MarkerValue $marker "backend"); ModelsDir = $modelsDir }
    }
    throw "could not provision any llama.cpp backend for windows-x64 at $Tag"
  }

  $bin = Find-LlamaServer $llamaDir
  New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
  Write-Host "==> llama.cpp backend: $committed  bin: $bin"
  Write-Host "==> llama.cpp models dir: $modelsDir  (drop your .gguf files here)"
  return @{ Bin = $bin; Backend = $committed; ModelsDir = $modelsDir }
}

# Provision before the mode-specific install so the env is ready for both the
# startup task and the desktop launch.
$llama = $null
if ($Runtime -eq "llamacpp") {
  $llamaTag = Resolve-LlamaCppTag $LlamaCppVersion $LlamaCppRepo
  if (-not $llamaTag) { throw "could not resolve llama.cpp version" }
  $llama = Install-LlamaCpp $LlamaCppBackend $llamaTag $LlamaCppRepo $mode
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

  # Point the launched tray at the provisioned engine. Process-scoped only; on a
  # later launch (reboot) the Rust default_bin()/default_models_dir() Windows
  # roots find the same install.
  if ($llama) {
    $env:LOCALLMOS_RUNTIME = "llamacpp"
    $env:LOCALLMOS_LLAMACPP_BIN = $llama.Bin
    $env:LOCALLMOS_LLAMACPP_MODELS_DIR = $llama.ModelsDir
    $env:LOCALLMOS_LLAMACPP_BACKEND = $llama.Backend
  }

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

  # Point the SYSTEM startup task at the provisioned engine (machine-scoped so it
  # survives reboots), and the current session so the enroll step below agrees.
  if ($llama) {
    [Environment]::SetEnvironmentVariable("LOCALLMOS_RUNTIME", "llamacpp", "Machine")
    [Environment]::SetEnvironmentVariable("LOCALLMOS_LLAMACPP_BIN", $llama.Bin, "Machine")
    [Environment]::SetEnvironmentVariable("LOCALLMOS_LLAMACPP_MODELS_DIR", $llama.ModelsDir, "Machine")
    [Environment]::SetEnvironmentVariable("LOCALLMOS_LLAMACPP_BACKEND", $llama.Backend, "Machine")
    $env:LOCALLMOS_RUNTIME = "llamacpp"
    $env:LOCALLMOS_LLAMACPP_BIN = $llama.Bin
    $env:LOCALLMOS_LLAMACPP_MODELS_DIR = $llama.ModelsDir
    $env:LOCALLMOS_LLAMACPP_BACKEND = $llama.Backend
  }

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
if ($Runtime -eq "llamacpp") {
  Write-Host ""
  Write-Host "==> Runtime: llama.cpp (llama-server, backend=$($llama.Backend)) — $($llama.Bin)"
  Write-Host "   Add a .gguf to $($llama.ModelsDir), then select it in the dashboard."
} elseif (-not (Get-Command ollama -ErrorAction SilentlyContinue)) {
  Write-Host ""
  Write-Host "!! Ollama was not detected on this machine."
  Write-Host "   LocalLMOS uses Ollama to run models locally. Install it from:"
  Write-Host "     https://ollama.com/download   (or: winget install Ollama.Ollama)"
  Write-Host "   Then pull a model, e.g.:  ollama pull llama3.2"
}
