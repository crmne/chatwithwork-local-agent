# Build the per-user MSI for one architecture.
#
#   pwsh packaging/windows/build-msi.ps1 -Version 1.2.3 -Arch x64 `
#     -Binary target\x86_64-pc-windows-msvc\release\cww.exe -Output dist\cww.msi
#
# cww-agent.exe must sit next to -Binary.
#
# Needs the WiX Toolset 5 command-line tool (`dotnet tool install --global
# wix --version 5.0.2`); the Util extension is added on first use.
param(
  [Parameter(Mandatory)] [string] $Version,
  [Parameter(Mandatory)] [ValidateSet("x64", "arm64")] [string] $Arch,
  [Parameter(Mandatory)] [string] $Binary,
  [Parameter(Mandatory)] [string] $Output
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$wixVersion = "5.0.2"

# MSI versions are numeric: 1.2.3-beta.1 installs as 1.2.3.
$numeric = $Version.Split("-")[0]

$source = Join-Path ([System.IO.Path]::GetTempPath()) ("cww-msi-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $source | Out-Null
try {
  Copy-Item $Binary (Join-Path $source "cww.exe")
  Copy-Item (Join-Path (Split-Path -Parent $Binary) "cww-agent.exe") (Join-Path $source "cww-agent.exe")
  foreach ($doc in "README.md", "PROTOCOL.md", "SECURITY.md", "LICENSE-MIT", "LICENSE-APACHE") {
    Copy-Item (Join-Path $root $doc) $source
  }
  $extensions = & wix extension list -g 2>$null
  if (-not ($extensions -match "WixToolset.Util.wixext")) {
    & wix extension add -g "WixToolset.Util.wixext/$wixVersion"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  }
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null
  & wix build (Join-Path $PSScriptRoot "cww.wxs") -arch $Arch `
    -ext WixToolset.Util.wixext `
    -d "Version=$numeric" -d "SourceDir=$source" `
    -o $Output
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  Write-Host "Built $Output"
} finally {
  Remove-Item -Recurse -Force $source
}
