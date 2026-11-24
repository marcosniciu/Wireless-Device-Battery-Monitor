# Samples two running programs side by side, the way Task Manager reports them, and prints a
# comparison: RAM in Task Manager ("Memory" = private working set), working set incl. shared DLLs,
# private bytes (commit), handles, threads and CPU time per minute.
# Usage: .\bench\compare.ps1 [-A batteryhub-taskbar] [-B BatteryHub] [-Seconds 180] [-Interval 2] [-CsvDir bench]
param(
    [string] $A = 'batteryhub-taskbar',
    [string] $B = 'BatteryHub',
    [int] $Seconds = 180,
    [int] $Interval = 2,
    [string] $CsvDir
)

function Sample([string] $name) {
    $p = Get-Process $name -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { return $null }
    $pws = (Get-Counter "\Process($name)\Working Set - Private" -ErrorAction SilentlyContinue).CounterSamples | Select-Object -First 1
    [pscustomobject]@{
        t         = [math]::Round(((Get-Date) - $p.StartTime).TotalSeconds)
        PrivWsKB  = [math]::Round($pws.CookedValue / 1KB)
        WsKB      = [math]::Round($p.WorkingSet64 / 1KB)
        PrivateKB = [math]::Round($p.PrivateMemorySize64 / 1KB)
        Handles   = $p.HandleCount
        Threads   = $p.Threads.Count
        CpuMs     = [math]::Round($p.TotalProcessorTime.TotalMilliseconds)
        Id        = $p.Id
        Path      = $p.Path
    }
}

$sa = @(); $sb = @()
$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end) {
    $x = Sample $A; if ($x) { $sa += $x }
    $y = Sample $B; if ($y) { $sb += $y }
    Start-Sleep -Seconds $Interval
}
if ($CsvDir) {
    $sa | Select-Object t, PrivWsKB, WsKB, PrivateKB, Handles, Threads, CpuMs | Export-Csv (Join-Path $CsvDir "$A.csv") -NoTypeInformation
    $sb | Select-Object t, PrivWsKB, WsKB, PrivateKB, Handles, Threads, CpuMs | Export-Csv (Join-Path $CsvDir "$B.csv") -NoTypeInformation
}

function Summary($s) {
    if (-not $s) { return $null }
    $sorted = $s.PrivWsKB | Sort-Object
    $same = $s | Where-Object { $_.Id -eq $s[-1].Id }   # one process (a self-restart starts a new one)
    $cpuMin = if ($same.Count -gt 1) { ($same[-1].CpuMs - $same[0].CpuMs) / 1000 / (($same[-1].t - $same[0].t) / 60) } else { 0 }
    [pscustomobject]@{
        Program        = Split-Path $s[-1].Path -Leaf
        Version        = (Get-Item $s[-1].Path).VersionInfo.ProductVersion
        FileKB         = [math]::Round((Get-Item $s[-1].Path).Length / 1KB)
        Samples        = $s.Count
        RamMedianMB    = [math]::Round($sorted[[int]($sorted.Count / 2)] / 1KB, 2)
        RamAvgMB       = [math]::Round(($s.PrivWsKB | Measure-Object -Average).Average / 1KB, 2)
        RamMaxMB       = [math]::Round(($s.PrivWsKB | Measure-Object -Maximum).Maximum / 1KB, 2)
        WorkingSetMB   = [math]::Round(($s.WsKB | Measure-Object -Average).Average / 1KB, 2)
        PrivateBytesMB = [math]::Round(($s.PrivateKB | Measure-Object -Average).Average / 1KB, 2)
        Handles        = ($s.Handles | Measure-Object -Maximum).Maximum
        Threads        = ($s.Threads | Measure-Object -Maximum).Maximum
        CpuSecPerMin   = [math]::Round($cpuMin, 3)
    }
}

Summary $sa | Format-List
Summary $sb | Format-List
