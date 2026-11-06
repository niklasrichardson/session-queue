#Requires -Version 7
<#
Starts the Codex and Antigravity CLI cq watchers listed in a JSON config, in this window.

- Each watcher starts with --takeover, so it replaces an older watcher of the same name.
- Each output line starts with the watcher's name.
- A watcher that exits is not restarted, because a restart loop with --takeover would fight
  another watcher of that name. Run this script again instead.
- Close the window, or press Ctrl+C, to stop every watcher.
- Check a watcher from anywhere with: cq status --as NAME

Config: -Config PATH, else $env:CQ_WATCHERS, else watchers.json next to this script.
See watchers.example.json and docs/WATCHERS.md.
#>
param(
    [string]$Config,
    # Close the window at the end without waiting for Enter.
    [switch]$NoWait
)

$ErrorActionPreference = 'Stop'

function Wait-ForEnter {
    if (-not $NoWait) { Read-Host 'Press Enter to close' | Out-Null }
}

function Stop-WithError([string]$Message) {
    Write-Host "start-watchers: $Message" -ForegroundColor Red
    Wait-ForEnter
    exit 1
}

# Find and read the config.
if (-not $Config) { $Config = $env:CQ_WATCHERS }
if (-not $Config) { $Config = Join-Path $PSScriptRoot 'watchers.json' }
if (-not (Test-Path -LiteralPath $Config -PathType Leaf)) { Stop-WithError "config file not found: $Config" }
try { $cfg = Get-Content -LiteralPath $Config -Raw | ConvertFrom-Json }
catch { Stop-WithError "config file is not valid JSON: $Config ($($_.Exception.Message))" }
if ($cfg -isnot [pscustomobject]) { Stop-WithError "the config must be a JSON object: $Config" }

# Check the cq executable.
$cq = if ($cfg.cq) { $cfg.cq } else { 'cq' }
if (-not (Get-Command $cq -CommandType Application -ErrorAction SilentlyContinue)) {
    Stop-WithError "cq executable not found: $cq (set ""cq"" in the config to its full path)"
}

# Check each watcher entry.
if ($cfg.watchers -isnot [array]) { Stop-WithError """watchers"" must be a list in $Config" }
$allowed = 'name', 'mode', 'id', 'prompt', 'bin', 'enabled'
$names = @{}
$watchers = [System.Collections.Generic.List[hashtable]]::new()
$i = 0
foreach ($w in $cfg.watchers) {
    $i++
    $label = if ($w.name) { "watcher '$($w.name)'" } else { "watcher #$i" }
    if ($w -isnot [pscustomobject]) { Stop-WithError "each entry in ""watchers"" must be an object" }
    $unknown = $w.PSObject.Properties.Name | Where-Object { $_ -notin $allowed }
    if ($unknown) { Stop-WithError "${label}: unknown field(s): $($unknown -join ', ')" }
    if ($w.name -isnot [string] -or -not $w.name.Trim()) { Stop-WithError "${label}: ""name"" must be a non-empty string" }
    if ($w.mode -notin 'codex', 'agy') { Stop-WithError "${label}: ""mode"" must be ""codex"" or ""agy""" }
    if ($w.id -isnot [string] -or -not $w.id.Trim()) { Stop-WithError "${label}: ""id"" must be the thread or conversation ID" }
    if ($null -ne $w.prompt -and ($w.prompt -isnot [string] -or -not $w.prompt.Trim())) {
        Stop-WithError "${label}: ""prompt"" must be a non-empty string, or left out"
    }
    if ($null -ne $w.bin -and ($w.bin -isnot [string] -or -not $w.bin.Trim())) {
        Stop-WithError "${label}: ""bin"" must be the path to codex or agy, or left out"
    }
    if ($null -ne $w.enabled -and $w.enabled -isnot [bool]) { Stop-WithError "${label}: ""enabled"" must be true or false" }
    if ($w.enabled -eq $false) { continue }
    if ($names.ContainsKey($w.name)) { Stop-WithError "${label}: the name is used twice; two watchers with --takeover would fight" }
    $names[$w.name] = $true

    $wargs = @("--to-$($w.mode)", $w.id)
    if ($w.prompt) { $wargs += @("--$($w.mode)-prompt", $w.prompt) }
    if ($w.bin) { $wargs += @("--$($w.mode)-bin", $w.bin) }
    $watchers.Add(@{ Name = $w.name; Args = $wargs })
}
if ($watchers.Count -eq 0) { Stop-WithError "no enabled watchers in $Config" }

# Start each watcher as a thread job, and print its output with its name.
$Host.UI.RawUI.WindowTitle = 'cq watchers'
$jobs = foreach ($w in $watchers) {
    Start-ThreadJob -Name $w.Name -ArgumentList $cq, $w.Name, $w.Args -ScriptBlock {
        param($cq, $name, $wargs)
        & $cq watch --as $name @wargs --takeover 2>&1 | ForEach-Object { "$_" }
        "exited with code $LASTEXITCODE"
    }
}
Write-Host "Started $($jobs.Count) watcher(s). Close this window to stop them." -ForegroundColor Green

try {
    while ($true) {
        # Check the states first, so that the last lines of a job that has just ended are printed.
        $allEnded = -not ($jobs | Where-Object { $_.State -in 'NotStarted', 'Running' })
        foreach ($j in $jobs) {
            Receive-Job $j 2>&1 | ForEach-Object { Write-Host "[$($j.Name)] $_" }
        }
        if ($allEnded) {
            Write-Host 'All watchers have exited. Run this script again to start them.' -ForegroundColor Yellow
            break
        }
        Start-Sleep -Seconds 2
    }
}
finally {
    $jobs | Stop-Job -ErrorAction SilentlyContinue
}
Wait-ForEnter
