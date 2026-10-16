# Development watcher. Leave this running in its own terminal while Claude works;
# press Ctrl+C to stop. Each new line written to .tools\dev\request.txt
# ("<id> <task> [task...]") runs scripts\check.ps1 with those tasks. Output goes to
# .tools\dev\result-<id>.log. It never prompts for or receives credentials.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dev = Join-Path $root '.tools\dev'
New-Item -ItemType Directory -Force $dev | Out-Null
$request = Join-Path $dev 'request.txt'
$lastFile = Join-Path $dev 'last-processed.txt'
$status = Join-Path $dev 'status.txt'
$last = if (Test-Path $lastFile) { (Get-Content $lastFile -Raw).Trim() } else { '' }
"idle $last" | Set-Content $status
Write-Host "TUICanvas watcher running in $root (Ctrl+C to stop)" -ForegroundColor Cyan
while ($true) {
    $line = ''
    if (Test-Path $request) { $line = "$(Get-Content $request -Raw -ErrorAction SilentlyContinue)".Trim() }
    if ($line -and $line -ne $last) {
        $id, $tasks = $line -split '\s+', 2
        if ($id -match '^\d+$' -and $tasks -match '^[a-z0-9-]+( [a-z0-9-]+)*$') {
            Write-Host "[$(Get-Date -Format HH:mm:ss)] #$id $tasks"
            "running $id" | Set-Content $status
            & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check.ps1') -Id $id -Tasks $tasks
            $code = $LASTEXITCODE
            Write-Host "[$(Get-Date -Format HH:mm:ss)] #$id finished (exit $code)" -ForegroundColor $(if ($code) { 'Yellow' } else { 'Green' })
            "done $id $code" | Set-Content $status
        } else {
            Write-Host "Ignored malformed request." -ForegroundColor Yellow
        }
        $last = $line
        $line | Set-Content $lastFile
    }
    Start-Sleep -Milliseconds 800
}
