<#
.SYNOPSIS
  Runs every DTO benchmark recorded in benchmarks/README.md on this machine.

.DESCRIPTION
  1. Engine thread scaling     (dto-bench engine, in-memory)
  2. Large enterprise records  (dto-bench engine --payload-kb 6.4 --chunk-size 2000)
  3. Batch size vs memory      (dto-bench engine --chunk-size N)
  4. End-to-end thread scaling (dto transform on generated files, RAYON_NUM_THREADS)
  5. Batching modes            (dto transform --chunk-size vs --max-memory, narrow and wide rows)

  Requires PowerShell 7+ (Windows, Linux or macOS) and a Rust toolchain.
  Generated input files go to benchmarks/data/ (git-ignored).
  Peak memory of child processes is read via .NET and may show 0 on macOS.

.EXAMPLE
  pwsh benchmarks/run_all.ps1          # full run, several minutes
  pwsh benchmarks/run_all.ps1 -Quick   # smaller sizes, about a minute
#>
param([switch]$Quick)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$data = Join-Path $PSScriptRoot "data"
$recipes = Join-Path $PSScriptRoot "recipes"
New-Item -ItemType Directory -Force $data | Out-Null

Write-Host "Building release binaries..."
Push-Location $root
try { cargo build --release -p dto_cli -p dto_bench 2>&1 | Out-Null } finally { Pop-Location }
$ext = if ($IsWindows) { ".exe" } else { "" }
$dto = Join-Path $root "target/release/dto$ext"
$bench = Join-Path $root "target/release/dto-bench$ext"

$cpus = [Environment]::ProcessorCount
$threads = @(1, 2, 4, 6, $cpus) | Where-Object { $_ -le $cpus } | Sort-Object -Unique
Write-Host "Logical CPUs: $cpus; thread counts: $($threads -join ', ')`n"

function Show($title, $rows) {
  Write-Host "== $title"
  $rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
}

# Runs dto-bench engine and extracts the summary numbers.
function Invoke-Engine([string[]]$benchArgs) {
  $text = & $bench engine @benchArgs 2>&1 | Out-String
  [pscustomobject]@{
    RecordsPerSec = [long]([regex]::Match($text, 'Average Throughput\s*:\s*(\d+)').Groups[1].Value)
    AvgTime       = [regex]::Match($text, 'Average Elapsed Time\s*:\s*(\S+)').Groups[1].Value
    PeakMB        = [double]([regex]::Match($text, 'Peak Working Set RSS\s*:\s*([\d.]+)').Groups[1].Value)
  }
}

# Runs a process, polling its peak working set. Returns seconds, peak MB, stderr.
function Measure-Process([string]$exe, [string[]]$argList, [int]$threadCount = 0) {
  if ($threadCount -gt 0) { $env:RAYON_NUM_THREADS = "$threadCount" } else { Remove-Item Env:RAYON_NUM_THREADS -ErrorAction SilentlyContinue }
  $out = Join-Path $data "out.json"
  $err = Join-Path $data "err.txt"
  try {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $exe -ArgumentList $argList -RedirectStandardOutput $out -RedirectStandardError $err -NoNewWindow -PassThru
    $peak = 0
    while (-not $p.HasExited) {
      try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
      Start-Sleep -Milliseconds 5
    }
    $sw.Stop()
    if ($p.ExitCode -ne 0) { throw "$exe failed: $(Get-Content $err -Raw)" }
  } finally {
    Remove-Item Env:RAYON_NUM_THREADS -ErrorAction SilentlyContinue
  }
  [pscustomobject]@{ Seconds = $sw.Elapsed.TotalSeconds; PeakMB = [long][math]::Round($peak / 1MB); Stderr = (Get-Content $err -Raw) }
}

function Get-DataFile([string]$kind, [int]$count) {
  $path = Join-Path $data "$kind-$count.json"
  if (-not (Test-Path $path)) { & $bench generate --kind $kind --count $count --output $path 2>&1 | Out-Null }
  # One untimed run so the first measurement is not skewed by a cold file cache.
  $null = Measure-Process $dto @("transform", "-s", (Join-Path $recipes "$kind.json"), "-i", $path)
  $path
}

# 1. Engine thread scaling
$sizes = if ($Quick) { @(10000, 50000) } else { @(10000, 50000, 100000, 250000) }
$rows = foreach ($n in $sizes) {
  foreach ($t in $threads) {
    $r = Invoke-Engine @("--count", "$n", "--threads", "$t")
    [pscustomobject]@{ Records = $n; Threads = $t; 'Records/s' = $r.RecordsPerSec; 'Avg time' = $r.AvgTime; 'Peak MB' = $r.PeakMB }
  }
}
Show "1. Engine only, all records in memory (0.3 KB records, 9-field recipe)" $rows

# 2. Large enterprise records
$n = if ($Quick) { 20000 } else { 100000 }
$rows = foreach ($t in $threads) {
  $r = Invoke-Engine @("--count", "$n", "--threads", "$t", "--payload-kb", "6.4", "--chunk-size", "2000")
  [pscustomobject]@{ Records = $n; Threads = $t; 'Records/s' = $r.RecordsPerSec; 'Peak MB' = $r.PeakMB }
}
Show "2. Engine only, 6.4 KB enterprise records, 2,000 per batch" $rows

# 3. Batch size vs memory
$n = if ($Quick) { 50000 } else { 250000 }
$all = Invoke-Engine @("--count", "$n")
$rows = @([pscustomobject]@{ 'Batch size' = "all at once"; 'Records/s' = $all.RecordsPerSec; 'Peak MB' = $all.PeakMB })
$rows += foreach ($cs in 500, 1000, 2000, 5000, 10000, 50000) {
  $r = Invoke-Engine @("--count", "$n", "--chunk-size", "$cs")
  [pscustomobject]@{ 'Batch size' = $cs; 'Records/s' = $r.RecordsPerSec; 'Peak MB' = $r.PeakMB }
}
Show "3. Engine only, $n records: batch size vs memory (all threads)" $rows

# 4. End-to-end thread scaling
$sizes = if ($Quick) { @(10000, 100000) } else { @(10000, 100000, 300000) }
$rows = foreach ($n in $sizes) {
  $file = Get-DataFile "narrow" $n
  foreach ($t in $threads) {
    $m = Measure-Process $dto @("transform", "-s", (Join-Path $recipes "narrow.json"), "-i", $file) $t
    [pscustomobject]@{ Records = $n; Threads = $t; Seconds = [math]::Round($m.Seconds, 2); 'Records/s' = [long]($n / $m.Seconds); 'Peak MB' = $m.PeakMB }
  }
}
Show "4. End to end: dto transform (read + parse + transform + write), default mode" $rows

# 5. Batching modes on narrow and wide rows
$cases = @(
  @{ Name = "narrow 0.3 KB"; Kind = "narrow"; Recipe = "narrow.json"; Count = $(if ($Quick) { 100000 } else { 300000 }) },
  @{ Name = "wide 3.9 KB"; Kind = "wide"; Recipe = "wide.json"; Count = $(if ($Quick) { 20000 } else { 50000 }) }
)
$modes = @(
  @{ Label = "--chunk-size 5000"; Args = @("--chunk-size", "5000") },
  @{ Label = "--max-memory 16MB"; Args = @("--max-memory", "16MB") },
  @{ Label = "--max-memory 64MB"; Args = @("--max-memory", "64MB") },
  @{ Label = "--max-memory 256MB"; Args = @("--max-memory", "256MB") }
)
$rows = foreach ($c in $cases) {
  $file = Get-DataFile $c.Kind $c.Count
  foreach ($mode in $modes) {
    $m = Measure-Process $dto (@("transform", "-s", (Join-Path $recipes $c.Recipe), "-i", $file, "--stats") + $mode.Args)
    $perBatch = [regex]::Match($m.Stderr, 'avg (\d+)').Groups[1].Value
    [pscustomobject]@{ Rows = "$($c.Count) $($c.Name)"; Mode = $mode.Label; Seconds = [math]::Round($m.Seconds, 2); 'Peak MB' = $m.PeakMB; 'Avg rows/batch' = $perBatch }
  }
}
Show "5. Batching modes: fixed row count vs memory cap" $rows
