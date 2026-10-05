# Run only on an isolated Windows CI runner, after building the MSI.
param([Parameter(Mandatory)] [string] $Msi, [switch] $StandardUser)
$ErrorActionPreference = "Stop"
if ($StandardUser) {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'This regression test must run without administrator rights'
  }
  $env:CWW_HOME = Join-Path $env:LOCALAPPDATA 'Cww smoke test\state & data'
}
$Msi = (Resolve-Path $Msi).Path
$dir = Join-Path $env:LOCALAPPDATA "Programs\Chat with Work Local Agent"
$programs = [Environment]::GetFolderPath("Programs")
$startup = Join-Path ([Environment]::GetFolderPath('Startup')) 'Chat with Work Local Agent.lnk'
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
  if ($StandardUser) {
    if (-not (Test-Path -LiteralPath $startup)) { throw 'No per-user startup fallback was created' }
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($startup)
    if ($shortcut.TargetPath -ne (Join-Path $dir 'cww-agent.exe')) { throw 'Startup points to the wrong agent' }
    # Exercise the same entry Explorer opens at login, after setup has exited.
    & (Join-Path $dir 'cww.exe') daemon stop
    if ($LASTEXITCODE -ne 0) { throw 'Could not stop the agent before the login test' }
    for ($i = 0; $i -lt 30; $i++) {
      $status = & (Join-Path $dir 'cww.exe') status
      if ($status -notmatch 'running \(pid') { break }
      Start-Sleep -Milliseconds 200
    }
    Start-Process -FilePath $startup
    $running = $false
    for ($i = 0; $i -lt 30; $i++) {
      $status = & (Join-Path $dir 'cww.exe') status
      if ($status -match 'running \(pid') { $running = $true; break }
      Start-Sleep -Seconds 1
    }
    if (-not $running) { throw 'The login shortcut did not start the agent' }
    # The desktop app's Start Local Agent action must also be repeatable.
    & (Join-Path $dir 'cww.exe') daemon install
    if ($LASTEXITCODE -ne 0) { throw 'Starting the agent again as a standard user failed' }
  }
} finally {
  Run-Msi "/x"
}
foreach ($binary in "cww.exe", "cww-app.exe", "cww-agent.exe") {
  if (Test-Path (Join-Path $dir $binary)) { throw "Uninstall left $binary" }
}
foreach ($shortcut in "Chat with Work.lnk", "Chat with Work (Terminal).lnk") {
  if (Test-Path (Join-Path $programs $shortcut)) { throw "Uninstall left $shortcut" }
}
if (Test-Path -LiteralPath $startup) { throw 'Uninstall left the startup shortcut' }
