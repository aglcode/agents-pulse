<#
.SYNOPSIS
  Measure AgentPulse idle memory and CPU on Windows, summed over the whole
  process tree (the app plus its msedgewebview2.exe children).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts/perf/measure.ps1 -Seconds 600

.NOTES
  Architecture §10: performance targets are benchmarks to measure, not assumptions.
  Record results in docs/perf.md with the build type, window state and date.
#>
param(
  [string]$ProcessName = "agents-pulse",
  [int]$Seconds = 60
)

$ErrorActionPreference = "Stop"

function Get-Tree([int]$RootId) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
  $ids = [System.Collections.Generic.List[int]]::new()
  $ids.Add($RootId)
  $i = 0
  while ($i -lt $ids.Count) {
    $parent = $ids[$i]
    $all | Where-Object { $_.ParentProcessId -eq $parent } | ForEach-Object { $ids.Add([int]$_.ProcessId) }
    $i++
  }
  return $ids
}

$root = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Sort-Object StartTime | Select-Object -First 1
if (-not $root) { throw "Process '$ProcessName' is not running." }

$ids = Get-Tree $root.Id
$procs = $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue } | Where-Object { $_ }
$cpu0 = ($procs | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
$t0 = Get-Date

Start-Sleep -Seconds $Seconds

$procs = $ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue } | Where-Object { $_ }
$cpu1 = ($procs | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
$elapsedMs = ((Get-Date) - $t0).TotalMilliseconds
$cores = [Environment]::ProcessorCount

$ws = ($procs | Measure-Object -Property WorkingSet64 -Sum).Sum
$private = ($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum
$appOnly = ($procs | Where-Object { $_.Id -eq $root.Id } | Select-Object -First 1).WorkingSet64

[pscustomobject]@{
  Processes          = $procs.Count
  WorkingSetMB       = [math]::Round($ws / 1MB, 1)
  PrivateMB          = [math]::Round($private / 1MB, 1)
  AppProcessOnlyMB   = [math]::Round($appOnly / 1MB, 1)
  CpuPctOfOneCore    = [math]::Round(100 * ($cpu1 - $cpu0) / $elapsedMs, 2)
  CpuPctOfMachine    = [math]::Round(100 * ($cpu1 - $cpu0) / ($elapsedMs * $cores), 3)
  SampleSeconds      = $Seconds
  LogicalCores       = $cores
} | Format-List
