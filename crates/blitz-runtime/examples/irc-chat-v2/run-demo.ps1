$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")).Path
$demo = Join-Path $repo "crates\blitz-runtime\examples\irc-chat-v2"
$buildTarget = Join-Path $repo "target\irc-chat-v2-build"
$builtBlitz = Join-Path $buildTarget "debug\blitz.exe"
$runId = [Guid]::NewGuid().ToString("N")
$runDirectory = Join-Path ([IO.Path]::GetTempPath()) "blitz-irc-chat-v2\$runId"
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

function Start-BlitzProcess($script, $module = $false) {
  $arguments = if ($module) { "run --module `"$script`"" } else { "`"$script`"" }
  Start-Process `
    -FilePath $blitz `
    -ArgumentList $arguments `
    -WorkingDirectory $repo
}

Start-BlitzProcess (Join-Path $demo "server.mjs") $true
$serverReady = $false
for ($attempt = 0; $attempt -lt 60; $attempt++) {
  try {
    $response = Invoke-WebRequest -UseBasicParsing -TimeoutSec 1 -Uri "http://127.0.0.1:8000/health"
    if ($response.StatusCode -eq 200) {
      $serverReady = $true
      break
    }
  } catch {
    Start-Sleep -Seconds 1
  }
}
if (-not $serverReady) {
  throw "IRC chat v2 server did not become ready within 60 seconds"
}
Start-BlitzProcess (Join-Path $demo "alice.mjs")
Start-BlitzProcess (Join-Path $demo "bob.mjs")
