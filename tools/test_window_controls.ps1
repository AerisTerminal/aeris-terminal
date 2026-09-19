<#
.SYNOPSIS
Exercises TradingPlot's real Win32 non-client hit-test and caption-action path.

.DESCRIPTION
Build the release diagnostics binary first. Run this script once for each launch mode and DPI
configured on the Windows test machine. The defaults perform the required 100 maximize/restore
transitions and 50 fresh-process launches.
#>
[CmdletBinding()]
param(
    [string]$BinaryPath = "target/release/tradingplot_desktop.exe",
    [ValidateSet("normal", "workspace-tabs", "multi-chart")]
    [string]$Mode = "normal",
    [ValidateRange(1, 1000)]
    [int]$MaximizeCycles = 100,
    [ValidateRange(1, 500)]
    [int]$FreshLaunches = 50,
    [ValidateRange(1, 300)]
    [int]$WarmupSeconds = 15,
    [string]$ReportPath = "local-data/evidence/window-controls.json",
    [int]$ExpectedDpi = 0
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class TradingPlotWindowControlsNative {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, UIntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, UIntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetPhysicalCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetPhysicalCursorPos(int x, int y);
    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, flags, time; public UIntPtr extraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public MOUSEINPUT mouse; }
    [DllImport("user32.dll")] public static extern uint SendInput(uint count, ref INPUT input, int size);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] private static extern bool AttachThreadInput(uint sourceThreadId, uint targetThreadId, bool attach);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int command);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    private delegate bool EnumWindowsCallback(IntPtr h, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint processId);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr h);
    public static IntPtr[] WindowsForProcess(uint processId) {
        var handles = new List<IntPtr>();
        EnumWindows(delegate(IntPtr h, IntPtr parameter) {
            uint owner;
            GetWindowThreadProcessId(h, out owner);
            if (owner == processId && IsWindowVisible(h)) handles.Add(h);
            return true;
        }, IntPtr.Zero);
        return handles.ToArray();
    }
    public static bool SendLeftButton(bool down) {
        var input = new INPUT { type = 0, mouse = new MOUSEINPUT { flags = down ? 0x0002U : 0x0004U } };
        return SendInput(1, ref input, Marshal.SizeOf(typeof(INPUT))) == 1;
    }
    public static bool SendMove() {
        var input = new INPUT { type = 0, mouse = new MOUSEINPUT { flags = 0x0001U } };
        return SendInput(1, ref input, Marshal.SizeOf(typeof(INPUT))) == 1;
    }
    public static bool ActivateWindow(IntPtr h) {
        var foreground = GetForegroundWindow();
        uint targetProcess;
        var targetThread = GetWindowThreadProcessId(h, out targetProcess);
        var currentThread = GetCurrentThreadId();
        var foregroundThread = foreground == IntPtr.Zero ? 0U : GetWindowThreadProcessId(foreground, out targetProcess);
        var attached = foregroundThread != 0 && foregroundThread != currentThread && foregroundThread != targetThread
            && AttachThreadInput(currentThread, foregroundThread, true);
        var activated = SetForegroundWindow(h) && BringWindowToTop(h);
        if (attached) AttachThreadInput(currentThread, foregroundThread, false);
        return activated && GetForegroundWindow() == h;
    }
}
"@

# Keep P/Invoke screen/client coordinates in the same physical-pixel space as the GPUI process.
[void][TradingPlotWindowControlsNative]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))

$WM_MOUSEMOVE = 0x0200
$WM_NCHITTEST = 0x0084
$WM_NCLBUTTONDOWN = 0x00A1
$WM_NCLBUTTONUP = 0x00A2
$WM_CLOSE = 0x0010
$HTCLIENT = 1
$HTCAPTION = 2
$HTMINBUTTON = 8
$HTMAXBUTTON = 9
$HTCLOSE = 20
$SW_RESTORE = 9

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function New-LParam([int]$X, [int]$Y) {
    $packed = (($Y -band 0xffff) -shl 16) -bor ($X -band 0xffff)
    return [IntPtr]::new([int]$packed)
}

function Get-LaunchArguments {
    switch ($Mode) {
        "normal" { return @() }
        "workspace-tabs" { return @("--workspace-tabs") }
        "multi-chart" { return @("--multi-chart") }
    }
}

function Start-TestWindow([string]$Lane) {
    [string[]]$launchArguments = @(Get-LaunchArguments)
    $script:LaunchIndex++
    $stderr = Join-Path $reportDirectory ("window-controls-{0:D3}-{1}.stderr.log" -f $script:LaunchIndex, $Lane)
    if ($launchArguments.Count -eq 0) {
        $process = Start-Process -FilePath $script:ResolvedBinary -RedirectStandardError $stderr -PassThru
    } else {
        $process = Start-Process -FilePath $script:ResolvedBinary -ArgumentList $launchArguments -RedirectStandardError $stderr -PassThru
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        Start-Sleep -Milliseconds 50
        $process.Refresh()
        if ($process.HasExited) { throw "Desktop exited before exposing a window (exit $($process.ExitCode))." }
        $handle = $process.MainWindowHandle
    } while ($handle -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $deadline)
    Assert-True ($handle -ne [IntPtr]::Zero) "Desktop did not expose a native window within 20 seconds."
    Start-Sleep -Milliseconds 250
    return @{ Process = $process; Handle = $handle; Stderr = $stderr }
}

function Get-ControlPoints([IntPtr]$Handle) {
    $rect = New-Object TradingPlotWindowControlsNative+RECT
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    do {
        if (([TradingPlotWindowControlsNative]::GetClientRect($Handle, [ref]$rect)) -and ($rect.Right -gt $rect.Left) -and ($rect.Bottom -gt $rect.Top)) {
            break
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    Assert-True (($rect.Right -gt $rect.Left) -and ($rect.Bottom -gt $rect.Top)) "GetClientRect did not expose a valid native client area after the window transition."
    $dpi = [int][TradingPlotWindowControlsNative]::GetDpiForWindow($Handle)
    $scale = $dpi / 96.0
    $width = $rect.Right - $rect.Left
    $titleBarY = [Math]::Round(21.0 * $scale)
    $headerY = [Math]::Round(65.0 * $scale)
    function Make-Point([int]$ClientX, [int]$ClientY) {
        $screen = New-Object TradingPlotWindowControlsNative+POINT
        $screen.X = $ClientX
        $screen.Y = $ClientY
        Assert-True ([TradingPlotWindowControlsNative]::ClientToScreen($Handle, [ref]$screen)) "ClientToScreen failed."
        return @{ ClientX = $ClientX; ClientY = $ClientY; ScreenX = $screen.X; ScreenY = $screen.Y }
    }
    return @{
        Dpi = $dpi
        Minimize = Make-Point ([Math]::Round($width - 115.0 * $scale)) $titleBarY
        Maximize = Make-Point ([Math]::Round($width - 69.0 * $scale)) $titleBarY
        Close = Make-Point ([Math]::Round($width - 23.0 * $scale)) $titleBarY
        Drag = Make-Point ([Math]::Round(30.0 * $scale)) $titleBarY
        Interactive = Make-Point ([Math]::Round(160.0 * $scale)) $headerY
    }
}

function Get-HitTest([IntPtr]$Handle, $Point) {
    # Move the real cursor so Windows emits the same client/non-client transition sequence as a
    # user. Synthetic WM_MOUSEMOVE alone does not maintain Win32's NC tracking state.
    [void][TradingPlotWindowControlsNative]::ActivateWindow($Handle)
    [void][TradingPlotWindowControlsNative]::BringWindowToTop($Handle)
    [void][TradingPlotWindowControlsNative]::SetActiveWindow($Handle)
    [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($Point.ScreenX, $Point.ScreenY)
    [void][TradingPlotWindowControlsNative]::SetCursorPos($Point.ScreenX, $Point.ScreenY)
    Start-Sleep -Milliseconds 25
    $actualCursor = New-Object TradingPlotWindowControlsNative+POINT
    [void][TradingPlotWindowControlsNative]::GetCursorPos([ref]$actualCursor)
    $cursorMoved = $actualCursor.X -eq $Point.ScreenX -and $actualCursor.Y -eq $Point.ScreenY
    if ($null -eq $script:CursorInjectionAvailable) { $script:CursorInjectionAvailable = $cursorMoved }
    if (-not $cursorMoved) {
        # Headless/remote Windows sessions may reject cursor injection. Exercise the same pinned
        # GPUI input callback directly in that case, using client coordinates.
        [void][TradingPlotWindowControlsNative]::SendMessage(
            $Handle, $WM_MOUSEMOVE, [UIntPtr]::Zero, (New-LParam $Point.ClientX $Point.ClientY))
        Start-Sleep -Milliseconds 25
    }
    $result = [TradingPlotWindowControlsNative]::SendMessage(
        $Handle, $WM_NCHITTEST, [UIntPtr]::Zero, (New-LParam $Point.ScreenX $Point.ScreenY))
    return $result.ToInt32()
}

function Assert-StableHitTest([IntPtr]$Handle, $Point, [int]$Expected, [string]$Name) {
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    $consecutive = 0
    $observed = New-Object 'System.Collections.Generic.List[int]'
    do {
        $actual = Get-HitTest $Handle $Point
        $observed.Add($actual)
        if ($actual -eq $Expected) { $consecutive++ } else { $consecutive = 0 }
        $requiredConsecutive = if ($script:CursorInjectionAvailable) { 3 } else { 1 }
    } while ($consecutive -lt $requiredConsecutive -and [DateTime]::UtcNow -lt $deadline)
    if ($consecutive -ne $requiredConsecutive) {
        Save-DiagnosticCapture $Handle
        throw "$Name did not stabilize at hit code $Expected within two seconds (observed $($observed -join ','))."
    }
}

function Save-DiagnosticCapture([IntPtr]$Handle) {
    $rect = New-Object TradingPlotWindowControlsNative+RECT
    if (-not [TradingPlotWindowControlsNative]::GetWindowRect($Handle, [ref]$rect)) { return }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) { return }
    $bitmap = New-Object Drawing.Bitmap $width,$height
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $failurePath = Join-Path $repositoryRoot "local-data/window-controls-failure.png"
        $bitmap.Save($failurePath, [Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Assert-HitTests([IntPtr]$Handle, [bool]$IncludeInteractive = $true) {
    $points = Get-ControlPoints $Handle
    if ($ExpectedDpi -gt 0) {
        Assert-True ($points.Dpi -eq $ExpectedDpi) "Expected DPI $ExpectedDpi but window reported $($points.Dpi)."
    }
    Assert-StableHitTest $Handle $points.Minimize $HTMINBUTTON "Minimize"
    Assert-StableHitTest $Handle $points.Maximize $HTMAXBUTTON "Maximize"
    Assert-StableHitTest $Handle $points.Close $HTCLOSE "Close"
    Assert-StableHitTest $Handle $points.Drag $HTCAPTION "Brand drag region"
    if ($IncludeInteractive) {
        Assert-StableHitTest $Handle $points.Interactive $HTCLIENT "Interactive title-bar control"
    }
    return $points
}

function Assert-GlyphPixels([IntPtr]$Handle, $Points) {
    # CopyFromScreen samples the composed DirectComposition result; an HWND GDI DC can remain
    # uniformly black even while GPUI's glyph is visibly presented.
    foreach ($name in @("Minimize", "Maximize", "Close")) {
        $point = $Points[$name]
        $deadline = [DateTime]::UtcNow.AddSeconds(2)
        $colorCount = 0
        do {
            $bitmap = New-Object Drawing.Bitmap 32,32
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.CopyFromScreen($point.ScreenX - 16, $point.ScreenY - 16, 0, 0, $bitmap.Size)
                $colors = New-Object 'System.Collections.Generic.HashSet[int]'
                for ($y = 0; $y -lt 32; $y++) {
                    for ($x = 0; $x -lt 32; $x++) {
                        [void]$colors.Add($bitmap.GetPixel($x, $y).ToArgb())
                    }
                }
                $colorCount = $colors.Count
            } finally {
                $graphics.Dispose()
                $bitmap.Dispose()
            }
            if ($colorCount -lt 2) { Start-Sleep -Milliseconds 25 }
        } while ($colorCount -lt 2 -and [DateTime]::UtcNow -lt $deadline)
        if ($colorCount -lt 2) { Save-DiagnosticCapture $Handle }
        Assert-True ($colorCount -ge 2) "$name glyph region at screen $($point.ScreenX),$($point.ScreenY) stayed visually uniform through the two-second readiness deadline."
    }
}

function Invoke-NativeClick([IntPtr]$Handle, [int]$HitCode, $Point) {
    Assert-StableHitTest $Handle $Point $HitCode "Native click target"
    if ($script:CursorInjectionAvailable) {
        [void][TradingPlotWindowControlsNative]::ActivateWindow($Handle)
        [void][TradingPlotWindowControlsNative]::SetActiveWindow($Handle)
        [void][TradingPlotWindowControlsNative]::SetCursorPos($Point.ScreenX, $Point.ScreenY)
        [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($Point.ScreenX, $Point.ScreenY)
        Assert-True ([TradingPlotWindowControlsNative]::SendMove()) "SendInput cursor move failed."
        Start-Sleep -Milliseconds 25
        Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($true)) "SendInput left-button down failed."
        Start-Sleep -Milliseconds 25
        Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($false)) "SendInput left-button up failed."
    } else {
        $screen = New-LParam $Point.ScreenX $Point.ScreenY
        $hit = [UIntPtr]::new([uint32]$HitCode)
        [void][TradingPlotWindowControlsNative]::SendMessage($Handle, $WM_NCLBUTTONDOWN, $hit, $screen)
        [void][TradingPlotWindowControlsNative]::SendMessage($Handle, $WM_NCLBUTTONUP, $hit, $screen)
    }
}

function Invoke-RealClientClick([IntPtr]$Handle, $Point) {
    Assert-StableHitTest $Handle $Point $HTCLIENT "Client click target"
    Assert-True ([bool]$script:CursorInjectionAvailable) "The required live lane cannot use synthetic client input."
    [void][TradingPlotWindowControlsNative]::ActivateWindow($Handle)
    [void][TradingPlotWindowControlsNative]::SetActiveWindow($Handle)
    [void][TradingPlotWindowControlsNative]::SetCursorPos($Point.ScreenX, $Point.ScreenY)
    [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($Point.ScreenX, $Point.ScreenY)
    Assert-True ([TradingPlotWindowControlsNative]::SendMove()) "SendInput header move failed."
    Start-Sleep -Milliseconds 25
    Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($true)) "SendInput header down failed."
    Start-Sleep -Milliseconds 25
    Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($false)) "SendInput header up failed."
}

function Invoke-CrossRelease([IntPtr]$Handle, [int]$DownHitCode, $DownPoint, [int]$UpHitCode, $UpPoint) {
    Assert-StableHitTest $Handle $DownPoint $DownHitCode "Cross-release press target"
    if ($script:CursorInjectionAvailable) {
        [void][TradingPlotWindowControlsNative]::ActivateWindow($Handle)
        [void][TradingPlotWindowControlsNative]::SetActiveWindow($Handle)
        [void][TradingPlotWindowControlsNative]::SetCursorPos($DownPoint.ScreenX, $DownPoint.ScreenY)
        [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($DownPoint.ScreenX, $DownPoint.ScreenY)
        Assert-True ([TradingPlotWindowControlsNative]::SendMove()) "SendInput cross-release move failed."
        Start-Sleep -Milliseconds 25
        Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($true)) "SendInput cross-release down failed."
        [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($UpPoint.ScreenX, $UpPoint.ScreenY)
        Start-Sleep -Milliseconds 25
        Assert-True ([TradingPlotWindowControlsNative]::SendLeftButton($false)) "SendInput cross-release up failed."
    } else {
        [void][TradingPlotWindowControlsNative]::SendMessage(
            $Handle, $WM_NCLBUTTONDOWN, [UIntPtr]::new([uint32]$DownHitCode),
            (New-LParam $DownPoint.ScreenX $DownPoint.ScreenY))
        [void][TradingPlotWindowControlsNative]::SendMessage(
            $Handle, $WM_NCLBUTTONUP, [UIntPtr]::new([uint32]$UpHitCode),
            (New-LParam $UpPoint.ScreenX $UpPoint.ScreenY))
    }
}

function Wait-State([scriptblock]$Predicate, [string]$Failure) {
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        if (& $Predicate) { return }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $Failure
}

function Close-TestProcess($TestProcess, [IntPtr]$InitialHandle, [string]$Failure) {
    [void][TradingPlotWindowControlsNative]::PostMessage($InitialHandle, $WM_CLOSE, [UIntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 250
    foreach ($handle in [TradingPlotWindowControlsNative]::WindowsForProcess([uint32]$TestProcess.Id)) {
        [void][TradingPlotWindowControlsNative]::PostMessage($handle, $WM_CLOSE, [UIntPtr]::Zero, [IntPtr]::Zero)
    }
    Assert-True ($TestProcess.WaitForExit(10000)) $Failure
}

function Read-LiveDiagnostics([string]$Path) {
    $snapshot = $null
    $lastRebuild = $null
    $lastTailRebuild = $null
    $tailUpdates = 0
    $tailReplaceRebuilds = 0
    $tailReplaceLayoutRebuilds = 0
    $tailReplaceFrameRebuilds = 0
    $chartMouseDowns = 0
    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        $stream = $null
        $reader = $null
        try {
            $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            $reader = New-Object IO.StreamReader($stream)
            $text = $reader.ReadToEnd()
            foreach ($line in ($text -split "`r?`n")) {
                try {
                    if ($line -match '^TRADINGPLOT_LIVE_SNAPSHOT (\{.*\})$') {
                        $snapshot = $Matches[1] | ConvertFrom-Json
                    } elseif ($line -match '^TRADINGPLOT_LIVE_UPDATE (\{.*\})$') {
                        $update = $Matches[1] | ConvertFrom-Json
                        if ($update.kind -eq "tail") { $tailUpdates++ }
                    } elseif ($line -match '^TRADINGPLOT_CHART_REBUILD (\{.*\})$') {
                        $rebuild = $Matches[1] | ConvertFrom-Json
                        $lastRebuild = $rebuild
                        if ($rebuild.data -eq "tail_replace") {
                            $lastTailRebuild = $rebuild
                            $tailReplaceRebuilds++
                            if ([bool]$rebuild.layout) {
                                $tailReplaceLayoutRebuilds++
                            } else {
                                $tailReplaceFrameRebuilds++
                            }
                        }
                    } elseif ($line -match '^TRADINGPLOT_CHART_MOUSE_DOWN ') {
                        $chartMouseDowns++
                    }
                } catch {
                    # Ignore a final line while the process is still appending it.
                }
            }
        } catch [IO.IOException] {
            # The next bounded poll retries while the redirected stream is being opened.
        } finally {
            if ($null -ne $reader) { $reader.Dispose() }
            if ($null -ne $stream) { $stream.Dispose() }
        }
    }
    return [pscustomobject]@{
        Snapshot = $snapshot
        LastRebuild = $lastRebuild
        LastTailRebuild = $lastTailRebuild
        TailUpdates = $tailUpdates
        TailReplaceRebuilds = $tailReplaceRebuilds
        TailReplaceLayoutRebuilds = $tailReplaceLayoutRebuilds
        TailReplaceFrameRebuilds = $tailReplaceFrameRebuilds
        ChartMouseDowns = $chartMouseDowns
    }
}

function Wait-LiveSnapshot($TestWindow) {
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        $TestWindow.Process.Refresh()
        if ($TestWindow.Process.HasExited) {
            throw "autoload_timeout: desktop exited before publishing a covering snapshot."
        }
        $diagnostics = Read-LiveDiagnostics $TestWindow.Stderr
        if ($null -ne $diagnostics.Snapshot -and [int]$diagnostics.Snapshot.bar_count -gt 0) {
            return $diagnostics
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "autoload_timeout: no covering chart snapshot was observed without a symbol-menu click."
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$script:ResolvedBinary = [IO.Path]::GetFullPath((Join-Path $repositoryRoot $BinaryPath))
$resolvedReport = [IO.Path]::GetFullPath((Join-Path $repositoryRoot $ReportPath))
Assert-True (Test-Path -LiteralPath $script:ResolvedBinary -PathType Leaf) "Release binary not found: $script:ResolvedBinary"
$reportDirectory = Split-Path -Parent $resolvedReport
if (-not (Test-Path -LiteralPath $reportDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $reportDirectory -Force | Out-Null
}

$active = $null
$liveDiagnosticsReportPath = $null
$observedDpi = 0
$script:CursorInjectionAvailable = $null
$script:LaunchIndex = 0
$originalLiveEvidence = $env:TRADINGPLOT_LIVE_EVIDENCE
$env:TRADINGPLOT_LIVE_EVIDENCE = "1"
$originalCursor = New-Object TradingPlotWindowControlsNative+POINT
[void][TradingPlotWindowControlsNative]::GetPhysicalCursorPos([ref]$originalCursor)
try {
    $active = Start-TestWindow "live"
    $autoloadDiagnostics = Wait-LiveSnapshot $active
    Assert-True ([int]$autoloadDiagnostics.Snapshot.interior_gaps -eq 0) "Published live snapshot contains $($autoloadDiagnostics.Snapshot.interior_gaps) interior timestamp gaps."
    Start-Sleep -Seconds $WarmupSeconds
    $streamingDiagnostics = Read-LiveDiagnostics $active.Stderr
    Assert-True ($streamingDiagnostics.TailUpdates -gt 0) "No live-tail publication was observed during the required $WarmupSeconds-second warmup."
    Assert-True ($streamingDiagnostics.TailReplaceRebuilds -gt 0) "No ordinary live-tail replacement reached a chart rebuild during warmup."
    Assert-True ($streamingDiagnostics.TailReplaceFrameRebuilds -gt 0) "Ordinary live-tail replacements remained in full-layout rebuilds during warmup."
    Assert-True ([bool]$script:CursorInjectionAvailable) "The required live lane could not inject a real Windows cursor click."

    $captionMouseDownsBefore = $streamingDiagnostics.ChartMouseDowns
    $warmPoints = Assert-HitTests $active.Handle
    Assert-GlyphPixels $active.Handle $warmPoints
    $beforeWarmMaximize = [TradingPlotWindowControlsNative]::IsZoomed($active.Handle)
    Invoke-NativeClick $active.Handle $HTMAXBUTTON $warmPoints.Maximize
    Wait-State { [TradingPlotWindowControlsNative]::IsZoomed($active.Handle) -ne $beforeWarmMaximize } "Warm maximize did not transition while live ticks were active."
    $warmPoints = Get-ControlPoints $active.Handle
    Invoke-NativeClick $active.Handle $HTMINBUTTON $warmPoints.Minimize
    Wait-State { [TradingPlotWindowControlsNative]::IsIconic($active.Handle) } "Warm minimize did not transition while live ticks were active."
    [void][TradingPlotWindowControlsNative]::ShowWindow($active.Handle, $SW_RESTORE)
    Wait-State { -not [TradingPlotWindowControlsNative]::IsIconic($active.Handle) } "Warm restore after minimize did not transition."
    $warmPoints = Get-ControlPoints $active.Handle
    Invoke-RealClientClick $active.Handle $warmPoints.Interactive
    Start-Sleep -Milliseconds 150
    $afterWarmClicks = Read-LiveDiagnostics $active.Stderr
    Assert-True ($afterWarmClicks.ChartMouseDowns -eq $captionMouseDownsBefore) "A caption or header click was classified as chart mouse down."
    $liveDiagnosticsReportPath = (Join-Path (Split-Path -Parent $ReportPath) ([IO.Path]::GetFileName($active.Stderr))).Replace('\', '/')

    $points = Assert-HitTests $active.Handle
    $observedDpi = $points.Dpi
    Assert-GlyphPixels $active.Handle $points

    for ($cycle = 0; $cycle -lt $MaximizeCycles; $cycle++) {
        $before = [TradingPlotWindowControlsNative]::IsZoomed($active.Handle)
        $points = Get-ControlPoints $active.Handle
        Invoke-NativeClick $active.Handle $HTMAXBUTTON $points.Maximize
        Wait-State { [TradingPlotWindowControlsNative]::IsZoomed($active.Handle) -ne $before } "Maximize cycle $cycle did not transition once."
        Start-Sleep -Milliseconds 100
        [void](Assert-HitTests $active.Handle $false)
    }

    $points = Get-ControlPoints $active.Handle
    $beforeCrossRelease = [TradingPlotWindowControlsNative]::IsZoomed($active.Handle)
    Invoke-CrossRelease $active.Handle $HTMAXBUTTON $points.Maximize $HTMINBUTTON $points.Minimize
    Start-Sleep -Milliseconds 100
    Assert-True ([TradingPlotWindowControlsNative]::IsZoomed($active.Handle) -eq $beforeCrossRelease) "Cross-button release changed window state."

    $points = Get-ControlPoints $active.Handle
    $beforeOutsideRelease = [TradingPlotWindowControlsNative]::IsZoomed($active.Handle)
    Invoke-CrossRelease $active.Handle $HTMAXBUTTON $points.Maximize $HTCAPTION $points.Drag
    Start-Sleep -Milliseconds 100
    Assert-True ([TradingPlotWindowControlsNative]::IsZoomed($active.Handle) -eq $beforeOutsideRelease) "Release outside the pressed caption changed window state."

    $points = Get-ControlPoints $active.Handle
    Invoke-NativeClick $active.Handle $HTMINBUTTON $points.Minimize
    Wait-State { [TradingPlotWindowControlsNative]::IsIconic($active.Handle) } "Minimize did not transition."
    [void][TradingPlotWindowControlsNative]::ShowWindow($active.Handle, $SW_RESTORE)
    Wait-State { -not [TradingPlotWindowControlsNative]::IsIconic($active.Handle) } "Restore after minimize did not transition."
    [void](Assert-HitTests $active.Handle)

    Close-TestProcess $active.Process $active.Handle "Desktop did not exit after WM_CLOSE; possible orphan or stalled retirement."
    $active = $null

    for ($launch = 0; $launch -lt $FreshLaunches; $launch++) {
        $fresh = Start-TestWindow "cold"
        try {
            $freshPoints = Assert-HitTests $fresh.Handle
            Assert-GlyphPixels $fresh.Handle $freshPoints
            Close-TestProcess $fresh.Process $fresh.Handle "Fresh launch $launch did not retire every window and exit."
        } finally {
            if (-not $fresh.Process.HasExited) { Stop-Process -Id $fresh.Process.Id -Force }
        }
    }

    [ordered]@{
        schema_version = 2
        evidence_scope = "tradingplot_live_chart_and_native_window_controls"
        mode = $Mode
        dpi = $observedDpi
        warmup_seconds = $WarmupSeconds
        autoload_without_symbol_switch = $true
        bar_count = [int]$autoloadDiagnostics.Snapshot.bar_count
        first_timestamp = [int64]$autoloadDiagnostics.Snapshot.first_timestamp
        last_timestamp = [int64]$autoloadDiagnostics.Snapshot.last_timestamp
        interior_timestamp_gaps = [int]$autoloadDiagnostics.Snapshot.interior_gaps
        live_tail_publications = $streamingDiagnostics.TailUpdates
        live_tail_rebuilds = $streamingDiagnostics.TailReplaceRebuilds
        live_tail_frame_only_rebuilds = $streamingDiagnostics.TailReplaceFrameRebuilds
        live_tail_full_layout_rebuilds = $streamingDiagnostics.TailReplaceLayoutRebuilds
        last_live_tail_rebuild_microseconds = [int64]$streamingDiagnostics.LastTailRebuild.micros
        last_live_tail_rebuild_layout = [bool]$streamingDiagnostics.LastTailRebuild.layout
        last_caption_click_classification = "non_client_caption"
        caption_click_logged_as_chart_mouse_down = $false
        warm_maximize_transition_verified = $true
        warm_minimize_restore_verified = $true
        warm_header_click_verified = $true
        maximize_restore_transitions = $MaximizeCycles
        fresh_process_launches = $FreshLaunches
        hit_tests_verified = $true
        glyph_pixels_verified = $true
        cross_button_release_cancelled = $true
        release_outside_cancelled = $true
        minimize_restore_verified = $true
        close_retirement_exit_verified = $true
        physical_cursor_injection = [bool]$script:CursorInjectionAvailable
        diagnostics_path = $liveDiagnosticsReportPath
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resolvedReport -Encoding UTF8
    Write-Host "Window-control conformance passed: $resolvedReport"
} finally {
    $env:TRADINGPLOT_LIVE_EVIDENCE = $originalLiveEvidence
    [void][TradingPlotWindowControlsNative]::SetPhysicalCursorPos($originalCursor.X, $originalCursor.Y)
    if ($null -ne $active -and -not $active.Process.HasExited) {
        Stop-Process -Id $active.Process.Id -Force
    }
}
