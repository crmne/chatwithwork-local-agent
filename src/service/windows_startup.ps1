$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
# A fresh profile may not have a Startup directory yet. Resolve its known
# location without requiring it to exist; installation creates it below.
$directory = [Environment]::GetFolderPath('Startup', 'DoNotVerify')
if ([string]::IsNullOrWhiteSpace($directory)) { throw 'Your Windows Startup folder could not be found.' }
$path = Join-Path $directory 'Chat with Work Local Agent.lnk'
if ($env:CWW_STARTUP_ACTION -eq 'remove') {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path
        [Console]::WriteLine($path)
    }
    exit 0
}
if ($env:CWW_STARTUP_ACTION -ne 'install') { throw 'Unknown startup action.' }
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($path)
$shortcut.TargetPath = $env:CWW_STARTUP_EXE
$shortcut.Arguments = $env:CWW_STARTUP_ARGS
$shortcut.WorkingDirectory = [IO.Path]::GetDirectoryName($env:CWW_STARTUP_EXE)
$shortcut.Description = 'Chat with Work Local Agent'
$shortcut.WindowStyle = 7
$shortcut.Save()
[Console]::WriteLine($path)
