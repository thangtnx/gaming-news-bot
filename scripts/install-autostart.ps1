param(
    [string]$TaskName
)

$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
$CargoToml = Join-Path $ProjectRoot "Cargo.toml"
$PackageName = (Select-String -Path $CargoToml -Pattern '^\s*name\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $PackageName) {
    $PackageName = Split-Path -Leaf $ProjectRoot
}
if (-not $TaskName) {
    $TaskName = $PackageName
}

$ExePath = Join-Path $ProjectRoot "target\release\$PackageName.exe"

if (-not (Test-Path $ExePath)) {
    Write-Host "Building release binary..."
    Push-Location $ProjectRoot
    cargo build --release
    Pop-Location
}

$EnvFile = Join-Path $ProjectRoot ".env"
if (-not (Test-Path $EnvFile)) {
    Write-Warning "Missing .env file. Copy .env.example to .env and set your bot token before running."
}

$Action = New-ScheduledTaskAction `
    -Execute $ExePath `
    -WorkingDirectory $ProjectRoot

$Trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$Settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero)

Register-ScheduledTask `
    -TaskName $TaskName `
    -Action $Action `
    -Trigger $Trigger `
    -Settings $Settings `
    -Description "$TaskName (auto-start on logon)" `
    -Force | Out-Null

Write-Host "Registered Scheduled Task '$TaskName' successfully."
Write-Host "The bot will start automatically in the background whenever you log on to Windows."
Write-Host "Start now: Start-ScheduledTask -TaskName '$TaskName'"
Write-Host "Remove:    Unregister-ScheduledTask -TaskName '$TaskName' -Confirm:`$false"
