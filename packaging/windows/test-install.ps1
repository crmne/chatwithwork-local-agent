# Run only on an isolated Windows CI runner, after building the MSI.
param([Parameter(Mandatory)] [string] $Msi)
$ErrorActionPreference = "Stop"
$Msi = (Resolve-Path $Msi).Path
$dir = Join-Path $env:LOCALAPPDATA "Programs\Chat with Work Local Agent"
$programs = [Environment]::GetFolderPath("Programs")
function Run-Msi([string] $action) {
  $process = Start-Process msiexec.exe -Wait -PassThru -ArgumentList $action, "`"$Msi`"", "/qn", "/norestart"
  if ($process.ExitCode -notin @(0, 3010)) { throw "msiexec $action failed: $($process.ExitCode)" }
}
try {
  Run-Msi "/i"
  foreach ($binary in "cww.exe", "cww-app.exe", "cww-agent.exe") {
    if (-not (Test-Path (Join-Path $dir $binary))) { throw "Missing $binary" }
  }
  foreach ($shortcut in "Chat with Work.lnk", "Chat with Work (Terminal).lnk") {
    if (-not (Test-Path (Join-Path $programs $shortcut))) { throw "Missing $shortcut" }
  }
  & (Join-Path $dir "cww.exe") --version
  if ($LASTEXITCODE -ne 0) { throw "cww failed" }
  & (Join-Path $dir "cww-app.exe") --version
  if ($LASTEXITCODE -ne 0) { throw "cww-app failed" }
  # The MSI must actually start the per-user daemon, not merely ship it.
  $running = $false
  for ($i = 0; $i -lt 30; $i++) {
    $status = & (Join-Path $dir "cww.exe") status
    if ($status -match "running \(pid") { $running = $true; break }
    Start-Sleep -Seconds 1
  }
  if (-not $running) { throw "The MSI did not start the agent" }
} finally {
  Run-Msi "/x"
}
foreach ($binary in "cww.exe", "cww-app.exe", "cww-agent.exe") {
  if (Test-Path (Join-Path $dir $binary)) { throw "Uninstall left $binary" }
}
foreach ($shortcut in "Chat with Work.lnk", "Chat with Work (Terminal).lnk") {
  if (Test-Path (Join-Path $programs $shortcut)) { throw "Uninstall left $shortcut" }
}
