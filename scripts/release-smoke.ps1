param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][int]$Port,
    [Parameter(Mandatory = $true)][string]$ExpectedVersion
)

$ErrorActionPreference = "Stop"

function Invoke-SidecarSmoke([int]$RunPort) {
    $arguments = @(
        "--demo", "--no-rbn", "--port", $RunPort,
        "--lofi-base", "http://127.0.0.1:9"
    )
    $process = Start-Process -FilePath $Binary -ArgumentList $arguments -PassThru
    try {
        $health = $null
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            try {
                $health = Invoke-RestMethod "http://127.0.0.1:$RunPort/healthz"
                break
            } catch {
                Start-Sleep -Milliseconds 250
            }
        }
        if ($null -eq $health) { throw "QSO Sidecar did not become healthy" }
        if (-not $health.ok -or $health.version -ne $ExpectedVersion) {
            throw "Unexpected health response"
        }

        $index = Invoke-WebRequest "http://127.0.0.1:$RunPort/" -UseBasicParsing
        $script = Invoke-WebRequest "http://127.0.0.1:$RunPort/app.js" -UseBasicParsing
        $state = Invoke-RestMethod "http://127.0.0.1:$RunPort/api/state"
        if ($index.Content -notmatch "QSO Sidecar") { throw "Dashboard HTML is missing" }
        if ($script.Content -notmatch "EventSource") { throw "Embedded app.js is missing" }
        if (-not $state.demo -or $state.spots_enabled) { throw "Unexpected demo state" }
    } finally {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id
            $process.WaitForExit()
        }
    }
}

& $Binary --version
if ($LASTEXITCODE -ne 0) { throw "--version failed" }
& $Binary --help | Out-Null
if ($LASTEXITCODE -ne 0) { throw "--help failed" }
Invoke-SidecarSmoke $Port
Invoke-SidecarSmoke ($Port + 1)
