# Memory and CPU: BatteryHub (Rust) vs. original BatteryHub (C++) 1.1.0

Measured on 2026-11-21 on Windows 11 (build 26300), x64, with the same devices connected: Logitech PRO 2 LIGHTSPEED, SteelSeries Arctis Nova 3P receiver and AirPods (Bluetooth). Both programs used their defaults: taskbar strip, battery refresh every minute.

Each program was sampled every 2 s with [`measure.ps1`](measure.ps1), which reports the numbers Task Manager shows. "Steady" means after start-up (t ≥ 30 s).

## Latest: side by side, v1.1 vs. C++ 1.1.0

Both programs running at the same time on the same PC (Windows 11 build 26300, x64), same devices connected (Logitech PRO 2 LIGHTSPEED receiver, SteelSeries Arctis Nova 3P dongle, Bluetooth), default settings (refresh every minute). The C++ app was given 75 s to start (it restarts itself once), then both were sampled every 2 s for 3 minutes with [`compare.ps1`](compare.ps1). Raw samples: [`side-by-side-rust-v1.1.csv`](side-by-side-rust-v1.1.csv), [`side-by-side-cpp-1.1.0.csv`](side-by-side-cpp-1.1.0.csv).

| | **Rust v1.1** | C++ 1.1.0 |
|---|---:|---:|
| RAM in Task Manager ("Memory" = private working set), median | **0.20 MB** | 4.66 MB |
| RAM in Task Manager, average | **0.33 MB** | 4.65 MB |
| RAM in Task Manager, max | **0.82 MB** | 4.92 MB |
| Working set incl. shared system DLLs, average | **1.27 MB** | 27.47 MB |
| Private bytes (commit), average | **4.59 MB** | 5.98 MB |
| CPU time per minute | **0.11 s** | 0.88 s |
| Threads (max) | **9** | 15 |
| Handles (max) | 367 | 375 |
| Program file | **500 KB** | 1,958 KB |

Handles stayed flat (366 -> 367) over the run, and the Rust process used 16 GDI objects, so there is no leak. Rust v1.1 has more handles than v1.0 (229) because of the newer features: device-change notifications, the hover tooltip and Bluetooth connection checks.

## RAM as Task Manager shows it (v1.1)

v1.1 hands memory back to Windows after every periodic job (battery read, Bluetooth check every 3 s, receiver check every 10 s) and caches its settings instead of reading the registry every second. Private working set of v1.1, sampled every second for 90 s after start-up:

| | v1.0.1.1 | **v1.1** |
|---|---:|---:|
| Median | 1.12 MB | **0.20 MB** |
| Average | 1.05 MB | **0.21 MB** |
| 90th percentile | - | **0.21 MB** |
| Max (briefly, during a full read) | 1.36 MB | 1.52 MB |

## RAM as Task Manager shows it (v1.0.1)

Task Manager's default "Memory" column is the **private working set**: memory that belongs to the program alone and is in RAM right now. Measured with the `\Process(*)\Working Set - Private` counter, 30 samples over 60 s for each program (measured one after the other, default settings):

| | **Rust** | C++ 1.1.0 |
|---|---:|---:|
| Private working set, average | **0.76 MB** | 4.15 MB |
| Private working set, max (during a read) | **1.39 MB** | 4.29 MB |

## Result

| | **Rust v1.0** | C++ 1.1.0 | Difference |
|---|---:|---:|---:|
| Program file size | **368 KB** | 1,958 KB | −81% |
| Working set (avg, steady) | **2.4 MB** | 29.4 MB | −92% |
| Working set during a read (peak) | **17.8 MB** | 29.8 MB | −40% |
| Private memory (avg, steady) | **3.2 MB** | 5.6 MB | −43% |
| Handles (max) | **229** | 392 | −42% |
| Threads (max) | **10** | 15 | −33% |
| CPU time per minute (steady) | **~0.02 s** | ~0.64 s | −97% |

## What the optimisation changed (Rust, before → after)

Both builds were measured back to back under the same conditions (3 minutes each, the C++ app running at the same time in both runs): [`baseline.csv`](baseline.csv) and [`optimized.csv`](optimized.csv).

| | Before | After |
|---|---:|---:|
| Working set (avg, steady) | 23.7 MB | **2.4 MB** |
| Private memory (avg, steady) | 3.9 MB | **3.2 MB** |
| Handles (max) | 303 | **229** |
| CPU time per minute | 0.37 s | **0.02 s** |

What was changed:

1. **Bluetooth LE listening only while reading.** The AirPods / Fast Pair broadcast watcher used to run all the time and handle every advertisement nearby (dozens per second). It now runs for the few seconds of each read and is stopped in between. This is the main CPU saving.
2. **No full device scan on every read.** The HID device list (which opens every HID device) is cached. It is rebuilt only at start-up, after a device connects or disconnects, on *Refresh devices*, and at least every 10 minutes. Container ids of devices are cached too.
3. **Bluetooth fallback reads only Bluetooth device nodes** (`BTHENUM`, `BTHLE`, `BTHLEDEVICE`) instead of enumerating every device node on the system.
4. **Windows.Gaming.Input is loaded only when an Xbox-type controller is present** (an XInput slot in use, or a Microsoft / GameSir gamepad over Bluetooth).
5. **No IME / text services**, since the program never takes text input.
6. **Memory used only during a read is handed back** to Windows after each read (`SetProcessWorkingSetSize`). This is why the steady working set is low. During a read it briefly rises again (peak 17.8 MB), mostly from shared system DLLs.

## Notes

- Working set includes shared system DLLs (GDI, WinRT, Bluetooth, and DLLs that tools such as Windhawk inject into every process). Private memory is what the program itself allocates.
- The C++ app does more: about 100 device drivers, battery history, an updater, brand colours and more options. Part of the difference is less code doing less work, not only the language.
- The C++ app restarted itself about 40 s after launch, so its steady state was measured on the restarted process. Its first-run samples are in [`cpp.csv`](cpp.csv). The first Rust run (before the optimisation) is in [`rust.csv`](rust.csv).

Reproduce:

```powershell
.\bench\measure.ps1 -Exe "$env:LOCALAPPDATA\Programs\BatteryHub\Taskbar\batteryhub-taskbar.exe" -Seconds 180 -Csv bench\optimized.csv
.\bench\measure.ps1 -Exe "C:\Program Files\BatteryHub\BatteryHub.exe" -Seconds 180 -Csv bench\cpp.csv
```
