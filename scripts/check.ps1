# Build/test/smoke tasks for TUICanvas. Run directly, e.g.:
#   powershell -ExecutionPolicy Bypass -File scripts\check.ps1 -Tasks "build test smoke"
# Writes .tools\dev\result-<id>.log. Live tasks are read-only and never print secrets.
param([string]$Id = 'manual', [string]$Tasks = 'build test')
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$dev = Join-Path $root '.tools\dev'
New-Item -ItemType Directory -Force $dev | Out-Null
$log = Join-Path $dev "result-$Id.log"
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (($env:Path -split ';') -notcontains $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
$env:CARGO_TERM_COLOR = 'never'
"TUICanvas check #$Id  tasks: $Tasks  started $(Get-Date -Format o)" | Set-Content $log -Encoding utf8

function Step([string]$label, [scriptblock]$command) {
    "`n=== $label ===" | Add-Content $log -Encoding utf8
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $global:LASTEXITCODE = 0
    # Stringify stderr records too, so Windows PowerShell 5.1 logs them as plain lines.
    & $command 2>&1 | ForEach-Object { "$_" } | Add-Content $log -Encoding utf8
    $code = $LASTEXITCODE
    "=== $label exit=$code ms=$($watch.ElapsedMilliseconds) ===" | Add-Content $log -Encoding utf8
    return $code
}

$exe = Join-Path $root 'target\release\tuicanvas.exe'
$python = Join-Path $root '.venv\Scripts\python.exe'
$final = 0
foreach ($task in ($Tasks -split ' ')) {
    $code = switch ($task) {
        'env'     { Step 'env' { $PSVersionTable.PSVersion.ToString(); rustc --version; cargo --version; git --version } }
        'build'   { Step 'build' { cargo build --release } }
        'test'    { Step 'test' { cargo test --release } }
        'smoke'   { Step 'smoke' { & $exe --smoke } }
        'dump'    { Step 'dump' { & $exe --dump $dev } }
        'pytest'  { Step 'pytest' { & $python -m unittest tools.test_auth_probe tools.test_auth_session tools.test_auth_ui tools.test_browser_bench } }
        'bench'   { Step 'bench' { & $exe --bench 10 } }
        'sysinfo' { Step 'sysinfo' { (Get-CimInstance Win32_Processor).Name; (Get-CimInstance Win32_OperatingSystem).Caption; '{0} GB RAM' -f [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB) } }
        'survey'  { Step 'survey' { & $python -m tools.survey_assignments } }
        'files-probe' { Step 'files-probe' { & $python -m tools.probe_files } }
        'bench-browser' { Step 'bench-browser' { & $python -m tools.browser_bench --runs 10 } }
        # Draws to a real console window (opens one briefly) to include terminal output cost.
        'bench-terminal' { Step 'bench-terminal' { Start-Process -FilePath $exe -ArgumentList '--bench', '5', '--terminal' -WorkingDirectory $root -Wait; Get-Content (Join-Path $root 'benchmarks\private\tui-bench-terminal.json') -TotalCount 12 } }
        'session' { Step 'session' { & $python -m tools.terminal_login --check } }
        'initial-commits' { Step 'initial-commits' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root '.tools\initial-commits.ps1') } }
        'gitident' { Step 'gitident' { 'name set: ' + [bool](git config --global user.name); 'email set: ' + [bool](git config --global user.email); git config --global user.name } }
        'gitstatus' { Step 'gitstatus' { git status --short; git log --format='%h %ad %s' --date=format:'%Y-%m-%d %H:%M' -n 15 } }
        # Paths come from .tools\dev\commit-paths.txt (one per line); message from commit-message.txt.
        'commit'  { Step 'commit' {
                $paths = @(Get-Content (Join-Path $dev 'commit-paths.txt') | Where-Object { $_.Trim() })
                git add -- @paths
                if ($LASTEXITCODE -eq 0) { git commit -F (Join-Path $dev 'commit-message.txt') -- @paths }
            } }
        'push'    { Step 'push' { git push } }
        default  { "Unknown task: $task" | Add-Content $log -Encoding utf8; 2 }
    }
    if ($code -ne 0) { $final = $code; break }
}
"`nEXIT $final  finished $(Get-Date -Format o)" | Add-Content $log -Encoding utf8
exit $final
