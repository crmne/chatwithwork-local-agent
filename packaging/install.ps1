# Install the Chat with Work Local Agent (cww) on Windows from the latest
# GitHub release, without administrator rights.
#
#   powershell -ExecutionPolicy Bypass -c "irm https://github.com/crmne/chatwithwork-local-agent/releases/latest/download/cww-installer.ps1 | iex"
#
# Downloads the MSI for this machine (x64 or arm64), checks it against the
# release's checksums.txt, and installs it for the current user: cww.exe on
# the PATH, a Start menu entry, and the daemon's logon task, idle until you
# pair. Set $env:CWW_VERSION = "1.2.3" for a specific release, or
# $env:CWW_DOWNLOAD_BASE to a mirror holding a release's files.
$ErrorActionPreference = "Stop"
$repo = "crmne/chatwithwork-local-agent"

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
  "AMD64" { "x86_64-pc-windows-msvc" }
  "ARM64" { "aarch64-pc-windows-msvc" }
  default { throw "No build for $($env:PROCESSOR_ARCHITECTURE) Windows" }
}
if ($env:CWW_DOWNLOAD_BASE) {
  # A mirror, or a local copy of a release for testing.
  $base = $env:CWW_DOWNLOAD_BASE.TrimEnd("/")
} elseif ($env:CWW_VERSION) {
  $base = "https://github.com/$repo/releases/download/v$($env:CWW_VERSION.TrimStart('v'))"
} else {
  $base = "https://github.com/$repo/releases/latest/download"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("cww-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  $ProgressPreference = "SilentlyContinue"
  $sums = Join-Path $tmp "checksums.txt"
  Invoke-WebRequest -UseBasicParsing "$base/checksums.txt" -OutFile $sums
  $line = Select-String -Path $sums -Pattern "cww-v\S+-$arch\.msi$" | Select-Object -First 1
  if (-not $line) { throw "The release has no installer for $arch" }
  $expected, $asset = ($line.Line -split "\s+", 2)
  $asset = $asset.Trim()
  Write-Host "Downloading $asset"
  $msi = Join-Path $tmp $asset
  Invoke-WebRequest -UseBasicParsing "$base/$asset" -OutFile $msi
  $actual = (Get-FileHash -Algorithm SHA256 $msi).Hash
  if ($actual -ne $expected.ToUpper()) { throw "Checksum mismatch for $asset" }

  $log = Join-Path $tmp "install.log"
  $process = Start-Process msiexec.exe -Wait -PassThru `
    -ArgumentList "/i", "`"$msi`"", "/qb!", "/norestart", "/l*v", "`"$log`""
  if ($process.ExitCode -ne 0) { throw "msiexec failed ($($process.ExitCode)); see $log" }

  $dir = Join-Path $env:LOCALAPPDATA "Programs\Chat with Work Local Agent"
  $env:Path = "$env:Path;$dir"
  Write-Host ""
  Write-Host "Installed $(& (Join-Path $dir 'cww.exe') --version) to $dir"
  Write-Host ""
  Write-Host "Next, in a new terminal:"
  Write-Host "  cww      # pair this computer and choose what to share"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
