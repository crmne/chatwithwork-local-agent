# Sign Windows files with a code signing certificate (Authenticode, SHA-256,
# RFC 3161 timestamp), for releases signed with a PFX instead of Azure
# Trusted Signing.
#
#   pwsh packaging/windows/sign.ps1 dist\cww.exe dist\cww-agent.exe
#
# Needs WINDOWS_CERTIFICATE_PFX (base64) and WINDOWS_CERTIFICATE_PASSWORD.
param([Parameter(Mandatory, ValueFromRemainingArguments)] [string[]] $Files)
$ErrorActionPreference = "Stop"
if (-not $env:WINDOWS_CERTIFICATE_PFX -or -not $env:WINDOWS_CERTIFICATE_PASSWORD) {
  throw "WINDOWS_CERTIFICATE_PFX and WINDOWS_CERTIFICATE_PASSWORD are required"
}
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
  Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe not found; install the Windows SDK" }
$pfx = Join-Path ([System.IO.Path]::GetTempPath()) ("cww-" + [guid]::NewGuid() + ".pfx")
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_PFX))
try {
  & $signtool.FullName sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com `
    /f $pfx /p $env:WINDOWS_CERTIFICATE_PASSWORD /d "Chat with Work Local Agent" $Files
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  & $signtool.FullName verify /pa $Files
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
  Remove-Item -Force $pfx
}
