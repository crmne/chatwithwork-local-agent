# Run only on an isolated, elevated Windows CI runner. Provision a fresh
# standard account so an administrator runner cannot mask startup failures.
param([Parameter(Mandatory)] [string] $Msi)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Use this account-provisioning test only on an isolated GitHub Actions runner' }
$name = 'cww-test-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$directory = Join-Path $env:PUBLIC $name
$taskName = 'Chat with Work Local Agent'
$createdUser = $false
$createdTask = $false
$installerPolicy = 'HKLM:\Software\Policies\Microsoft\Windows\Installer'
$previousDisableMsi = Get-ItemPropertyValue -Path $installerPolicy -Name DisableMSI -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $directory | Out-Null
try {
  # Windows Server runners default to blocking unmanaged per-user MSI
  # packages (1625). Exercise the desktop Windows policy without granting
  # the test user elevation. Restore the runner setting in finally.
  # https://learn.microsoft.com/en-us/windows/win32/msi/disablemsi
  New-Item -Path $installerPolicy -Force | Out-Null
  New-ItemProperty -Path $installerPolicy -Name DisableMSI -PropertyType DWord -Value 0 -Force | Out-Null
  Copy-Item -LiteralPath (Resolve-Path $Msi).Path -Destination (Join-Path $directory 'cww-test.msi')
  Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'test-install.ps1') -Destination $directory
  $password = ConvertTo-SecureString ('Cww!' + [Guid]::NewGuid().ToString('N')) -AsPlainText -Force
  New-LocalUser -Name $name -Password $password -AccountNeverExpires | Out-Null
  $createdUser = $true
  # Resolve the built-in group by SID on non-English Windows too.
  $users = Get-LocalGroup -SID 'S-1-5-32-545'
  Add-LocalGroupMember -Group $users -Member $name
  # Reproduce a task left by an elevated installer. The standard account
  # must neither replace nor delete this other user's task.
  $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Highest
  $action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\cmd.exe" -Argument '/c exit 0'
  Register-ScheduledTask -TaskName $taskName -Principal $principal -Action $action -Force | Out-Null
  $createdTask = $true
  Disable-ScheduledTask -TaskName $taskName | Out-Null
  # Leave ProcessStartInfo.Environment untouched. .NET then passes a null
  # environment to CreateProcessWithLogonW, which builds the new user's
  # login environment. Start-Process copies the administrator's instead.
  $start = New-Object Diagnostics.ProcessStartInfo
  $start.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
  $start.UserName = $name
  $start.Domain = $env:COMPUTERNAME
  $start.Password = $password
  $start.LoadUserProfile = $true
  $start.UseShellExecute = $false
  $start.WorkingDirectory = $directory
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "$directory\test-install.ps1", '-Msi', "$directory\cww-test.msi", '-StandardUser')) {
    $start.ArgumentList.Add($argument)
  }
  $process = [Diagnostics.Process]::Start($start)
  $stdout = $process.StandardOutput.ReadToEndAsync()
  $stderr = $process.StandardError.ReadToEndAsync()
  if (-not $process.WaitForExit(180000)) {
    Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('cww.exe', 'cww-agent.exe', 'powershell.exe', 'schtasks.exe', 'msiexec.exe') } |
      Select-Object Name, ProcessId, ParentProcessId, CommandLine | Format-List | Out-String | Write-Host
    $process.Kill($true)
    $process.WaitForExit(5000) | Out-Null
    if ($stdout.Wait(5000)) { Write-Host ($stdout.GetAwaiter().GetResult()) }
    if ($stderr.Wait(5000)) { Write-Host ($stderr.GetAwaiter().GetResult()) }
    $sid = (Get-LocalUser -Name $name).SID.Value
    $profile = Get-ItemPropertyValue -Path "HKLM:\Software\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid" -Name ProfileImagePath
    Get-ChildItem -Path "$profile\AppData\Local\Temp\cww-msi-*.log" | ForEach-Object {
      Write-Host "Installer log: $($_.Name)"
      Get-Content -LiteralPath $_.FullName -Tail 150 | Write-Host
    }
    throw 'Standard-user installation timed out'
  }
  Write-Host ($stdout.GetAwaiter().GetResult())
  Write-Host ($stderr.GetAwaiter().GetResult())
  if ($process.ExitCode -ne 0) { throw "Standard-user installation failed: $($process.ExitCode)" }
  $process.Dispose()
  Get-ScheduledTask -TaskName $taskName -ErrorAction Stop | Out-Null
} finally {
  if ($null -eq $previousDisableMsi) {
    Remove-ItemProperty -Path $installerPolicy -Name DisableMSI
  } else {
    Set-ItemProperty -Path $installerPolicy -Name DisableMSI -Value $previousDisableMsi
  }
  if ($createdTask) { Unregister-ScheduledTask -TaskName $taskName -Confirm:$false }
  if ($createdUser) { Remove-LocalUser -Name $name }
  Remove-Item -LiteralPath $directory -Recurse -Force
}
