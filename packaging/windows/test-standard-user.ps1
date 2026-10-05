# Run only on an isolated, elevated Windows CI runner. Provision a fresh
# standard account so an administrator runner cannot mask startup failures.
param([Parameter(Mandatory)] [string] $Msi)
$ErrorActionPreference = 'Stop'
$name = 'cww-test-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$directory = Join-Path $env:PUBLIC $name
$taskName = 'Chat with Work Local Agent'
$createdUser = $false
$createdTask = $false
New-Item -ItemType Directory -Path $directory | Out-Null
try {
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
  $credential = New-Object Management.Automation.PSCredential("$env:COMPUTERNAME\$name", $password)
  $process = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
    -Credential $credential -LoadUserProfile -Wait -PassThru `
    -ArgumentList '-NoLogo', '-NoProfile', '-NonInteractive', '-File', "`"$directory\test-install.ps1`"", '-Msi', "`"$directory\cww-test.msi`"", '-StandardUser' `
    -RedirectStandardOutput "$directory\stdout.log" -RedirectStandardError "$directory\stderr.log"
  Get-Content -LiteralPath "$directory\stdout.log"
  Get-Content -LiteralPath "$directory\stderr.log"
  if ($process.ExitCode -ne 0) { throw "Standard-user installation failed: $($process.ExitCode)" }
  Get-ScheduledTask -TaskName $taskName -ErrorAction Stop | Out-Null
} finally {
  if ($createdTask) { Unregister-ScheduledTask -TaskName $taskName -Confirm:$false }
  if ($createdUser) { Remove-LocalUser -Name $name }
  Remove-Item -LiteralPath $directory -Recurse -Force
}
