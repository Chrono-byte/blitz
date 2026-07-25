# SPDX-License-Identifier: MPL-2.0
$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")).Path
$demo = Join-Path $repo "crates\blitz-runtime\examples\irc-chat"
$buildTarget = Join-Path $repo "target\irc-chat-build"
$builtBlitz = Join-Path $buildTarget "debug\blitz.exe"
$runId = [Guid]::NewGuid().ToString("N")
$runDirectory = Join-Path ([IO.Path]::GetTempPath()) "blitz-irc-chat\$runId"
$blitz = Join-Path $runDirectory "blitz.exe"

Push-Location $repo
try {
  & cargo build -p blitz-cli --target-dir $buildTarget
  if ($LASTEXITCODE -ne 0) {
    throw "cargo build -p blitz-cli failed with exit code $LASTEXITCODE"
  }
} finally {
  Pop-Location
}

if (-not (Test-Path -LiteralPath $builtBlitz -PathType Leaf)) {
  throw "Expected Blitz executable at $builtBlitz"
}

$null = New-Item -ItemType Directory -Path $runDirectory -Force
Copy-Item -LiteralPath $builtBlitz -Destination $blitz

function Start-BlitzProcess($script) {
  Start-Process `
    -FilePath $blitz `
    -ArgumentList "`"$script`"" `
    -WorkingDirectory $repo
}

Start-BlitzProcess (Join-Path $demo "server.js")
Start-Sleep -Seconds 2
Start-BlitzProcess (Join-Path $demo "alice.js")
Start-BlitzProcess (Join-Path $demo "bob.js")
