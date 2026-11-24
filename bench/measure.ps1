# Measures the memory and CPU use of a running program, the way Task Manager reports it.
# Usage: .\bench\measure.ps1 -Exe <path> [-Seconds 120] [-Interval 2] [-Csv out.csv]
# Starts the program, samples it every -Interval seconds for -Seconds, then stops it and prints a
# summary: working set (avg / max), private bytes (avg / max), peak working set, handles, threads
# and CPU time. Run it for each program separately, so they do not compete for the same devices.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [int] $Seconds = 120,
    [int] $Interval = 2,
    [string] $Csv
)

$p = Start-Process $Exe -PassThru
$samples = @()
$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end -and -not $p.HasExited) {
    Start-Sleep -Seconds $Interval
    $p.Refresh()
    $samples += [pscustomobject]@{
        t          = [math]::Round(((Get-Date) - $p.StartTime).TotalSeconds)
        WorkingKB  = [math]::Round($p.WorkingSet64 / 1KB)
        PrivateKB  = [math]::Round($p.PrivateMemorySize64 / 1KB)
        PeakWSKB   = [math]::Round($p.PeakWorkingSet64 / 1KB)
        Handles    = $p.HandleCount
        Threads    = $p.Threads.Count
        CpuMs      = [math]::Round($p.TotalProcessorTime.TotalMilliseconds)
    }
}
$p.Refresh()
$cpu = $p.TotalProcessorTime.TotalMilliseconds
if (-not $p.HasExited) { Stop-Process $p -Force }
if ($Csv) { $samples | Export-Csv $Csv -NoTypeInformation }

$ws = $samples.WorkingKB | Measure-Object -Average -Maximum -Minimum
$pr = $samples.PrivateKB | Measure-Object -Average -Maximum
[pscustomobject]@{
    Program        = Split-Path $Exe -Leaf
    FileKB         = [math]::Round((Get-Item $Exe).Length / 1KB)
    Samples        = $samples.Count
    WorkingAvgMB   = [math]::Round($ws.Average / 1KB, 2)
    WorkingMaxMB   = [math]::Round($ws.Maximum / 1KB, 2)
    PrivateAvgMB   = [math]::Round($pr.Average / 1KB, 2)
    PrivateMaxMB   = [math]::Round($pr.Maximum / 1KB, 2)
    PeakWorkingMB  = [math]::Round(($samples.PeakWSKB | Measure-Object -Maximum).Maximum / 1KB, 2)
    HandlesMax     = ($samples.Handles | Measure-Object -Maximum).Maximum
    ThreadsMax     = ($samples.Threads | Measure-Object -Maximum).Maximum
    CpuSeconds     = [math]::Round($cpu / 1000, 2)
} | Format-List
