<#
.SYNOPSIS
Exercises Axiusflow's real Win32 non-client hit-test and caption-action path.

.DESCRIPTION
Build the release diagnostics binary first. Run this script once for each launch mode and DPI
configured on the Windows test machine. The defaults perform the required 100 maximize/restore
transitions and 50 fresh-process launches.
#>
[CmdletBinding()]
param(
    [string]$BinaryPath = "target/release/axiusflow_desktop.exe",
    [ValidateSet("normal", "workspace-tabs", "multi-chart")]
    [string]$Mode = "normal",
    [ValidateRange(1, 1000)]
    [int]$MaximizeCycles = 100,
    [ValidateRange(1, 500)]
    [int]$FreshLaunches = 50,
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
public static class AxiusflowWindowControlsNative {
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
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
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
}
"@

# Keep P/Invoke screen/client coordinates in the same physical-pixel space as the GPUI process.
[void][AxiusflowWindowControlsNative]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))

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

function Start-TestWindow {
    [string[]]$launchArguments = @(Get-LaunchArguments)
    if ($launchArguments.Count -eq 0) {
        $process = Start-Process -FilePath $script:ResolvedBinary -PassThru
    } else {
        $process = Start-Process -FilePath $script:ResolvedBinary -ArgumentList $launchArguments -PassThru
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
    return @{ Process = $process; Handle = $handle }
}

function Get-ControlPoints([IntPtr]$Handle) {
    $rect = New-Object AxiusflowWindowControlsNative+RECT
    Assert-True ([AxiusflowWindowControlsNative]::GetClientRect($Handle, [ref]$rect)) "GetClientRect failed."
    $dpi = [int][AxiusflowWindowControlsNative]::GetDpiForWindow($Handle)
    $scale = $dpi / 96.0
    $width = $rect.Right - $rect.Left
    $y = [Math]::Round(21.0 * $scale)
    function Make-Point([int]$ClientX, [int]$ClientY) {
        $screen = New-Object AxiusflowWindowControlsNative+POINT
        $screen.X = $ClientX
        $screen.Y = $ClientY
        Assert-True ([AxiusflowWindowControlsNative]::ClientToScreen($Handle, [ref]$screen)) "ClientToScreen failed."
        return @{ ClientX = $ClientX; ClientY = $ClientY; ScreenX = $screen.X; ScreenY = $screen.Y }
    }
    return @{
        Dpi = $dpi
        Minimize = Make-Point ([Math]::Round($width - 115.0 * $scale)) $y
        Maximize = Make-Point ([Math]::Round($width - 69.0 * $scale)) $y
        Close = Make-Point ([Math]::Round($width - 23.0 * $scale)) $y
        Drag = Make-Point ([Math]::Round(30.0 * $scale)) $y
        Interactive = Make-Point ([Math]::Round(160.0 * $scale)) $y
    }
}

function Get-HitTest([IntPtr]$Handle, $Point) {
    # Move the real cursor so Windows emits the same client/non-client transition sequence as a
    # user. Synthetic WM_MOUSEMOVE alone does not maintain Win32's NC tracking state.
    [void][AxiusflowWindowControlsNative]::SetForegroundWindow($Handle)
    [void][AxiusflowWindowControlsNative]::SetPhysicalCursorPos($Point.ScreenX, $Point.ScreenY)
    Start-Sleep -Milliseconds 25
    $actualCursor = New-Object AxiusflowWindowControlsNative+POINT
    [void][AxiusflowWindowControlsNative]::GetPhysicalCursorPos([ref]$actualCursor)
    $cursorMoved = $actualCursor.X -eq $Point.ScreenX -and $actualCursor.Y -eq $Point.ScreenY
    if ($null -eq $script:CursorInjectionAvailable) { $script:CursorInjectionAvailable = $cursorMoved }
    if (-not $cursorMoved) {
        # Headless/remote Windows sessions may reject cursor injection. Exercise the same pinned
        # GPUI input callback directly in that case, using client coordinates.
        [void][AxiusflowWindowControlsNative]::SendMessage(
            $Handle, $WM_MOUSEMOVE, [UIntPtr]::Zero, (New-LParam $Point.ClientX $Point.ClientY))
        Start-Sleep -Milliseconds 25
    }
    $result = [AxiusflowWindowControlsNative]::SendMessage(
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
    $rect = New-Object AxiusflowWindowControlsNative+RECT
    if (-not [AxiusflowWindowControlsNative]::GetWindowRect($Handle, [ref]$rect)) { return }
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

function Assert-HitTests([IntPtr]$Handle) {
    $points = Get-ControlPoints $Handle
    if ($ExpectedDpi -gt 0) {
        Assert-True ($points.Dpi -eq $ExpectedDpi) "Expected DPI $ExpectedDpi but window reported $($points.Dpi)."
    }
    Assert-StableHitTest $Handle $points.Minimize $HTMINBUTTON "Minimize"
    Assert-StableHitTest $Handle $points.Maximize $HTMAXBUTTON "Maximize"
    Assert-StableHitTest $Handle $points.Close $HTCLOSE "Close"
    Assert-StableHitTest $Handle $points.Drag $HTCAPTION "Brand drag region"
    Assert-StableHitTest $Handle $points.Interactive $HTCLIENT "Interactive title-bar control"
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
        Assert-True ($colorCount -ge 2) "$name glyph region at screen $($point.ScreenX),$($point.ScreenY) stayed visually uniform through the two-second readiness deadline."
    }
}

function Invoke-NativeClick([IntPtr]$Handle, [int]$HitCode, $Point) {
    Assert-StableHitTest $Handle $Point $HitCode "Native click target"
    $screen = New-LParam $Point.ScreenX $Point.ScreenY
    $hit = [UIntPtr]::new([uint32]$HitCode)
    [void][AxiusflowWindowControlsNative]::SendMessage($Handle, $WM_NCLBUTTONDOWN, $hit, $screen)
    [void][AxiusflowWindowControlsNative]::SendMessage($Handle, $WM_NCLBUTTONUP, $hit, $screen)
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
    [void][AxiusflowWindowControlsNative]::PostMessage($InitialHandle, $WM_CLOSE, [UIntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 250
    foreach ($handle in [AxiusflowWindowControlsNative]::WindowsForProcess([uint32]$TestProcess.Id)) {
        [void][AxiusflowWindowControlsNative]::PostMessage($handle, $WM_CLOSE, [UIntPtr]::Zero, [IntPtr]::Zero)
    }
    Assert-True ($TestProcess.WaitForExit(10000)) $Failure
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
$observedDpi = 0
$script:CursorInjectionAvailable = $null
$originalCursor = New-Object AxiusflowWindowControlsNative+POINT
[void][AxiusflowWindowControlsNative]::GetPhysicalCursorPos([ref]$originalCursor)
try {
    $active = Start-TestWindow
    $points = Assert-HitTests $active.Handle
    $observedDpi = $points.Dpi
    Assert-GlyphPixels $active.Handle $points

    for ($cycle = 0; $cycle -lt $MaximizeCycles; $cycle++) {
        $before = [AxiusflowWindowControlsNative]::IsZoomed($active.Handle)
        $points = Get-ControlPoints $active.Handle
        Invoke-NativeClick $active.Handle $HTMAXBUTTON $points.Maximize
        Wait-State { [AxiusflowWindowControlsNative]::IsZoomed($active.Handle) -ne $before } "Maximize cycle $cycle did not transition once."
        Start-Sleep -Milliseconds 100
        [void](Assert-HitTests $active.Handle)
    }

    $points = Get-ControlPoints $active.Handle
    $beforeCrossRelease = [AxiusflowWindowControlsNative]::IsZoomed($active.Handle)
    [void][AxiusflowWindowControlsNative]::SendMessage($active.Handle, $WM_NCLBUTTONDOWN, [UIntPtr]::new([uint32]$HTMAXBUTTON), (New-LParam $points.Maximize.ScreenX $points.Maximize.ScreenY))
    [void][AxiusflowWindowControlsNative]::SendMessage($active.Handle, $WM_NCLBUTTONUP, [UIntPtr]::new([uint32]$HTMINBUTTON), (New-LParam $points.Minimize.ScreenX $points.Minimize.ScreenY))
    Start-Sleep -Milliseconds 100
    Assert-True ([AxiusflowWindowControlsNative]::IsZoomed($active.Handle) -eq $beforeCrossRelease) "Cross-button release changed window state."

    $points = Get-ControlPoints $active.Handle
    $beforeOutsideRelease = [AxiusflowWindowControlsNative]::IsZoomed($active.Handle)
    [void][AxiusflowWindowControlsNative]::SendMessage($active.Handle, $WM_NCLBUTTONDOWN, [UIntPtr]::new([uint32]$HTMAXBUTTON), (New-LParam $points.Maximize.ScreenX $points.Maximize.ScreenY))
    [void][AxiusflowWindowControlsNative]::SendMessage($active.Handle, $WM_NCLBUTTONUP, [UIntPtr]::new([uint32]$HTCAPTION), (New-LParam $points.Drag.ScreenX $points.Drag.ScreenY))
    Start-Sleep -Milliseconds 100
    Assert-True ([AxiusflowWindowControlsNative]::IsZoomed($active.Handle) -eq $beforeOutsideRelease) "Release outside the pressed caption changed window state."

    $points = Get-ControlPoints $active.Handle
    Invoke-NativeClick $active.Handle $HTMINBUTTON $points.Minimize
    Wait-State { [AxiusflowWindowControlsNative]::IsIconic($active.Handle) } "Minimize did not transition."
    [void][AxiusflowWindowControlsNative]::ShowWindow($active.Handle, $SW_RESTORE)
    Wait-State { -not [AxiusflowWindowControlsNative]::IsIconic($active.Handle) } "Restore after minimize did not transition."
    [void](Assert-HitTests $active.Handle)

    Close-TestProcess $active.Process $active.Handle "Desktop did not exit after WM_CLOSE; possible orphan or stalled retirement."
    $active = $null

    for ($launch = 0; $launch -lt $FreshLaunches; $launch++) {
        $fresh = Start-TestWindow
        try {
            $freshPoints = Assert-HitTests $fresh.Handle
            Assert-GlyphPixels $fresh.Handle $freshPoints
            Close-TestProcess $fresh.Process $fresh.Handle "Fresh launch $launch did not retire every window and exit."
        } finally {
            if (-not $fresh.Process.HasExited) { Stop-Process -Id $fresh.Process.Id -Force }
        }
    }

    [ordered]@{
        schema_version = 1
        evidence_scope = "axiusflow_native_window_controls"
        mode = $Mode
        dpi = $observedDpi
        maximize_restore_transitions = $MaximizeCycles
        fresh_process_launches = $FreshLaunches
        hit_tests_verified = $true
        glyph_pixels_verified = $true
        cross_button_release_cancelled = $true
        release_outside_cancelled = $true
        minimize_restore_verified = $true
        close_retirement_exit_verified = $true
        physical_cursor_injection = [bool]$script:CursorInjectionAvailable
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resolvedReport -Encoding UTF8
    Write-Host "Window-control conformance passed: $resolvedReport"
} finally {
    [void][AxiusflowWindowControlsNative]::SetPhysicalCursorPos($originalCursor.X, $originalCursor.Y)
    if ($null -ne $active -and -not $active.Process.HasExited) {
        Stop-Process -Id $active.Process.Id -Force
    }
}
