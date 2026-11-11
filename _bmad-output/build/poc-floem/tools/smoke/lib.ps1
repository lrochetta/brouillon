# Brouillon GUI smoke harness - shared helpers (PowerShell 5.1, no external dependency).
#
# SAFETY CONTRACT
#   * Keystrokes are sent ONLY after Assert-Focus proved that the foreground
#     window belongs to the process we launched. Any mismatch throws
#     'FOCUS_LOST' and the scenario is abandoned (nothing is sent elsewhere).
#   * Screenshots copy the screen rectangle of OUR window only.
#   * Every scenario has a hard 60 s budget and always kills the process.

Set-StrictMode -Version 2
# The first-run screens would sit on top of every scenario: off unless a scenario asks for them.
if (-not $env:BROUILLON_KEEP_FIRSTRUN) { $env:BROUILLON_NO_FIRSTRUN = '1' }
$ErrorActionPreference = 'Stop'

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

[StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Explicit)] public struct INPUTUNION {
  [FieldOffset(0)] public MOUSEINPUT mi;
  [FieldOffset(0)] public KEYBDINPUT ki;
}
[StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }

public static class W32 {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] public static extern short VkKeyScan(char c);
  [DllImport("user32.dll")] public static extern uint MapVirtualKey(uint code, uint type);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr h);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();

  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, IntPtr extra);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  // Largest visible top-level window of a process (the editor window, not helper windows).
  public static IntPtr BigWindow(uint pid, int minW, int minH) {
    IntPtr best = IntPtr.Zero; long bestArea = 0;
    EnumWindows(delegate (IntPtr h, IntPtr l) {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != pid || !IsWindowVisible(h)) return true;
      RECT r; if (!GetWindowRect(h, out r)) return true;
      int w = r.Right - r.Left, ht = r.Bottom - r.Top;
      if (w >= minW && ht >= minH && (long)w * ht > bestArea) { bestArea = (long)w * ht; best = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }

  public static uint Key(ushort vk, bool up, bool ext, bool unicode, ushort scanOrChar) {
    INPUT[] a = new INPUT[1];
    a[0].type = 1;
    a[0].u.ki.wVk = unicode ? (ushort)0 : vk;
    a[0].u.ki.wScan = unicode ? scanOrChar : (ushort)MapVirtualKey(vk, 0);
    uint f = 0;
    if (up) f |= 0x0002;
    if (ext) f |= 0x0001;
    if (unicode) f |= 0x0004;
    a[0].u.ki.dwFlags = f;
    return SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
Add-Type -AssemblyName System.Drawing
[void][W32]::SetProcessDPIAware()

$script:SmokeRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$script:OutDir = Join-Path $script:SmokeRoot 'out'
$script:Deadline = $null
$script:Current = $null   # the app handle of the running scenario
$script:Apps = New-Object System.Collections.ArrayList

function Get-SmokeOut { New-Item -ItemType Directory -Force $script:OutDir | Out-Null; $script:OutDir }

# ---------------------------------------------------------------- process

function Start-App {
    param([string]$Exe, [string[]]$AppArgs = @(), [string]$WorkDir, [string]$Tag)
    $out = Get-SmokeOut
    $err = Join-Path $out "$Tag.stderr.log"
    $so = Join-Path $out "$Tag.stdout.log"
    Remove-Item $err, $so -ErrorAction SilentlyContinue
    # Process.Start (not Start-Process: it can stall for many seconds with redirections).
    # stderr/stdout are pumped to files by a .NET task, no PowerShell runspace needed.
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ($AppArgs -join ' ')
    $psi.WorkingDirectory = $WorkDir
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardOutput = $true
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = [Diagnostics.Process]::Start($psi)
    $spawnMs = $sw.ElapsedMilliseconds   # CreateProcess itself took ~26 s in the dev sandbox (not the app); window time is measured from here
    $sw.Restart()
    $fe = New-Object IO.FileStream($err, 'Create', 'Write', 'ReadWrite', 1)
    $fo = New-Object IO.FileStream($so, 'Create', 'Write', 'ReadWrite', 1)
    $t1 = $p.StandardError.BaseStream.CopyToAsync($fe)
    $t2 = $p.StandardOutput.BaseStream.CopyToAsync($fo)
    $app = [pscustomobject]@{ Proc = $p; Stderr = $err; Tag = $Tag; Watch = $sw; WindowMs = $null; Hwnd = [IntPtr]::Zero; SpawnMs = $spawnMs; Pumps = @($t1, $t2, $fe, $fo) }
    $script:Current = $app
    [void]$script:Apps.Add($app)
    if ($null -ne $script:Deadline) { $script:Deadline = $script:Deadline.AddMilliseconds($spawnMs) }   # spawn stall does not eat the 60 s budget
    $app
}

function Wait-Window {
    param($App, [int]$TimeoutMs = 20000)
    while ($App.Watch.ElapsedMilliseconds -lt $TimeoutMs) {
        if ($App.Proc.HasExited) { throw "PROCESS_EXITED before window (exit code $($App.Proc.ExitCode))" }
        $h = [W32]::BigWindow([uint32]$App.Proc.Id, 300, 200)
        if ($h.ToInt64() -ne 0) {
            $App.Hwnd = $h
            $App.WindowMs = $App.Watch.ElapsedMilliseconds
            return $App.WindowMs
        }
        Start-Sleep -Milliseconds 20
    }
    throw "NO_WINDOW within $TimeoutMs ms"
}

function Get-Mem {
    param($App)
    $App.Proc.Refresh()
    [pscustomobject]@{ WorkingSetMB = [math]::Round($App.Proc.WorkingSet64 / 1MB, 1); PeakMB = [math]::Round($App.Proc.PeakWorkingSet64 / 1MB, 1) }
}

function Stop-App {
    param($App)
    if ($null -eq $App) { return }
    try {
        if (-not $App.Proc.HasExited) {
            [void]$App.Proc.CloseMainWindow()
            [void]$App.Proc.WaitForExit(5000)
        }
    } catch {}
    try { if (-not $App.Proc.HasExited) { $App.Proc.Kill(); [void]$App.Proc.WaitForExit(3000) } } catch {}
}

function Stop-AppGraceful {
    param($App)
    $how = 'closed cleanly'
    try {
        [void]$App.Proc.CloseMainWindow()
        if (-not $App.Proc.WaitForExit(5000)) { $how = 'did NOT exit within 5 s (killed)'; $App.Proc.Kill(); [void]$App.Proc.WaitForExit(3000) }
    } catch {}
    $how
}

function Kill-App {
    param($App)
    try { if (-not $App.Proc.HasExited) { $App.Proc.Kill(); [void]$App.Proc.WaitForExit(3000) } } catch {}
}

function Read-Stderr {
    param($App)
    try {
        $fs = [IO.File]::Open($App.Stderr, 'Open', 'Read', 'ReadWrite')
        $sr = New-Object IO.StreamReader($fs)
        $t = $sr.ReadToEnd(); $sr.Close(); $t
    } catch { '' }
}

# ---------------------------------------------------------------- focus + input

function Test-Deadline {
    if ($null -ne $script:Deadline -and (Get-Date) -gt $script:Deadline) { throw 'SCENARIO_TIMEOUT (60 s budget)' }
}

function Get-ForegroundPid {
    $h = [W32]::GetForegroundWindow()
    if ($h.ToInt64() -eq 0) { return 0 }
    [uint32]$pid2 = 0
    [void][W32]::GetWindowThreadProcessId($h, [ref]$pid2)
    [int]$pid2
}

function Assert-Focus {
    param($App)
    Test-Deadline
    if ($App.Proc.HasExited) { throw 'PROCESS_EXITED' }
    $fg = Get-ForegroundPid
    if ($fg -ne $App.Proc.Id) { throw "FOCUS_LOST (foreground pid $fg, expected $($App.Proc.Id)); nothing sent" }
}

function Set-AppFocus {
    param($App)
    if ([W32]::IsIconic($App.Hwnd)) { [void][W32]::ShowWindow($App.Hwnd, 9) }
    for ($i = 0; $i -lt 15; $i++) {
        [void][W32]::SetForegroundWindow($App.Hwnd)
        Start-Sleep -Milliseconds 100
        if ((Get-ForegroundPid) -eq $App.Proc.Id) { return }
        if ($i -ge 2) {
            # Legit foreground-lock workaround that sends NO keystroke anywhere:
            # share input state with the current foreground thread for one call.
            $fgh = [W32]::GetForegroundWindow()
            [uint32]$dummy = 0
            $fgThread = [W32]::GetWindowThreadProcessId($fgh, [ref]$dummy)
            $me = [W32]::GetCurrentThreadId()
            if ($fgThread -ne 0 -and $fgThread -ne $me) {
                [void][W32]::AttachThreadInput($me, $fgThread, $true)
                [void][W32]::BringWindowToTop($App.Hwnd)
                [void][W32]::SetForegroundWindow($App.Hwnd)
                [void][W32]::AttachThreadInput($me, $fgThread, $false)
            }
            Start-Sleep -Milliseconds 150
            if ((Get-ForegroundPid) -eq $App.Proc.Id) { return }
        }
    }
    $fgn = try { (Get-Process -Id (Get-ForegroundPid)).ProcessName } catch { '?' }
    throw "FOCUS_NOT_ACQUIRED (foreground is '$fgn'; the OS refused to give us the foreground; nothing sent)"
}

$script:VK = @{
    'enter' = 0x0D; 'tab' = 0x09; 'esc' = 0x1B; 'back' = 0x08; 'delete' = 0x2E; 'space' = 0x20
    'left' = 0x25; 'up' = 0x26; 'right' = 0x27; 'down' = 0x28; 'home' = 0x24; 'end' = 0x23
    'pgup' = 0x21; 'pgdn' = 0x22; 'ctrl' = 0x11; 'shift' = 0x10; 'alt' = 0x12; 'comma' = 0xBC
}
1..12 | ForEach-Object { $script:VK["f$_"] = 0x6F + $_ }
$script:ExtVk = @(0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E)

function Resolve-Vk([string]$name) {
    $n = $name.ToLower()
    if ($script:VK.ContainsKey($n)) { return [uint16]$script:VK[$n] }
    if ($n.Length -eq 1) {
        $c = $n[0]
        if ($c -ge 'a' -and $c -le 'z') { return [uint16][int][char]$n.ToUpper() }
        if ($c -ge '0' -and $c -le '9') { return [uint16][int]$c }
    }
    throw "unknown key '$name'"
}

function Send-Vk([uint16]$vk, [bool]$up) {
    $ext = $script:ExtVk -contains [int]$vk
    [void][W32]::Key($vk, $up, $ext, $false, 0)
}

# Send-Chord $App 'ctrl+shift+f'   (modifiers: ctrl shift alt; last token is the key)
function Send-Chord {
    param($App, [string]$Chord, [int]$PauseMs = 120)
    Assert-Focus $App
    $parts = $Chord.ToLower().Split('+')
    # A single-token chord ('x', 'enter') has NO modifier. `$parts[0..-1]` is (0,-1) in PowerShell,
    # which used to press the key itself twice as a "modifier": one send arrived 3 times (OS auto-repeat).
    $mods = @(if ($parts.Length -gt 1) { $parts[0..($parts.Length - 2)] | Where-Object { $_ } })
    $key = Resolve-Vk $parts[-1]
    foreach ($m in $mods) { Send-Vk (Resolve-Vk $m) $false; Start-Sleep -Milliseconds 15 }
    Send-Vk $key $false; Start-Sleep -Milliseconds 25
    Send-Vk $key $true
    [array]::Reverse($mods)
    foreach ($m in $mods) { Start-Sleep -Milliseconds 15; Send-Vk (Resolve-Vk $m) $true }
    Start-Sleep -Milliseconds $PauseMs
}

function Send-Text {
    param($App, [string]$Text, [int]$CharDelayMs = 12)
    foreach ($ch in $Text.ToCharArray()) {
        Assert-Focus $App
        if ($ch -eq "`n") { Send-Vk 0x0D $false; Send-Vk 0x0D $true; Start-Sleep -Milliseconds $CharDelayMs; continue }
        if ($ch -eq "`r") { continue }
        $r = [W32]::VkKeyScan($ch)
        $vk = [uint16]($r -band 0xFF); $st = ($r -shr 8) -band 0xFF
        if ($r -eq -1 -or $st -gt 3) {
            [void][W32]::Key(0, $false, $false, $true, [uint16][int]$ch)
            [void][W32]::Key(0, $true, $false, $true, [uint16][int]$ch)
        } else {
            if ($st -band 1) { Send-Vk 0x10 $false }
            Send-Vk $vk $false; Send-Vk $vk $true
            if ($st -band 1) { Send-Vk 0x10 $true }
        }
        Start-Sleep -Milliseconds $CharDelayMs
    }
    Start-Sleep -Milliseconds 80
}

# Click at LOGICAL (DPI-independent) coordinates relative to the top-left of the
# window's outer frame (the same origin as Save-Shot images, divided by the DPI scale).
function Send-Click {
    param($App, [double]$LX, [double]$LY, [switch]$Double)
    Assert-Focus $App
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($App.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($App.Hwnd, [ref]$r) }
    $scale = [W32]::GetDpiForWindow($App.Hwnd) / 96.0
    $x = [int]($r.Left + $LX * $scale); $y = [int]($r.Top + $LY * $scale)
    if ($x -lt $r.Left -or $x -ge $r.Right -or $y -lt $r.Top -or $y -ge $r.Bottom) { throw "CLICK_OUTSIDE_WINDOW ($x,$y)" }
    [void][W32]::SetCursorPos($x, $y)
    Start-Sleep -Milliseconds 60
    Assert-Focus $App
    $n = 1; if ($Double) { $n = 2 }
    for ($i = 0; $i -lt $n; $i++) {
        [W32]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40
        [W32]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60
    }
    Start-Sleep -Milliseconds 150
}

# Press at (X1,Y1), move in steps to (X2,Y2), release. LOGICAL coordinates like Send-Click.
function Send-Drag {
    param($App, [double]$X1, [double]$Y1, [double]$X2, [double]$Y2, [int]$Steps = 12)
    Assert-Focus $App
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($App.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($App.Hwnd, [ref]$r) }
    $scale = [W32]::GetDpiForWindow($App.Hwnd) / 96.0
    foreach ($pt in @(@($X1, $Y1), @($X2, $Y2))) {
        $x = [int]($r.Left + $pt[0] * $scale); $y = [int]($r.Top + $pt[1] * $scale)
        if ($x -lt $r.Left -or $x -ge $r.Right -or $y -lt $r.Top -or $y -ge $r.Bottom) { throw "DRAG_OUTSIDE_WINDOW ($x,$y)" }
    }
    [void][W32]::SetCursorPos([int]($r.Left + $X1 * $scale), [int]($r.Top + $Y1 * $scale))
    Start-Sleep -Milliseconds 80
    Assert-Focus $App
    [W32]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le $Steps; $i++) {
        Assert-Focus $App
        $x = [int]($r.Left + ($X1 + ($X2 - $X1) * $i / $Steps) * $scale)
        $y = [int]($r.Top + ($Y1 + ($Y2 - $Y1) * $i / $Steps) * $scale)
        [void][W32]::SetCursorPos($x, $y); Start-Sleep -Milliseconds 25
    }
    [W32]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 200
}

# ---------------------------------------------------------------- capture

function Save-Shot {
    param($App, [string]$Name, [int]$SettleMs = 500)
    Start-Sleep -Milliseconds $SettleMs
    $out = Get-SmokeOut
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($App.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($App.Hwnd, [ref]$r) }
    $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
    if ($w -le 0 -or $h -le 0) { return $null }
    $bmp = New-Object Drawing.Bitmap($w, $h)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object Drawing.Size($w, $h)))
    $g.Dispose()
    $path = Join-Path $out "$Name.png"
    $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    $path
}

# ---------------------------------------------------------------- scenarios

$script:Results = New-Object System.Collections.ArrayList

# Invoke-Scenario 'a' 'title' { param($S) ... }   - scriptblock returns a detail string,
# throws on failure. Whatever happens the app is killed afterwards.
function Invoke-Scenario {
    param([string]$Id, [string]$Title, [scriptblock]$Body)
    $script:Deadline = (Get-Date).AddSeconds(60)
    $script:Current = $null
    $status = 'KO'; $detail = ''
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        $detail = & $Body
        $status = 'OK'
    } catch {
        $msg = $_.Exception.Message
        $detail = $msg
        if ($msg -like 'FOCUS_*') { $status = 'ABORTED(focus)' }
        elseif ($msg -like 'NOT_TESTABLE*') { $status = 'NOT_TESTED' }
    } finally {
        foreach ($a2 in $script:Apps) { Kill-App $a2 }
        $script:Apps.Clear()
        $script:Deadline = $null
    }
    $row = [pscustomobject]@{ Id = $Id; Title = $Title; Status = $status; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Detail = "$detail" }
    [void]$script:Results.Add($row)
    Write-Host ("[{0}] {1,-14} {2} ({3}s) {4}" -f $Id, $status, $Title, $row.Seconds, $row.Detail)
}

function New-TestEnv {
    param([string]$Root, [string]$Name)
    $d = Join-Path $Root $Name
    if (Test-Path $d) { Remove-Item -Recurse -Force $d }
    New-Item -ItemType Directory -Force (Join-Path $d 'settings') | Out-Null
    New-Item -ItemType Directory -Force (Join-Path $d 'files') | Out-Null
    $d
}

function Launch-Brouillon {
    param([string]$Exe, [string]$Dir, [string[]]$Files = @(), [string[]]$Extra = @('--new-window'), [string]$Tag)
    $a = @('--settings-dir', ($Dir + '\settings')) + $Extra + $Files
    $app = Start-App -Exe $Exe -AppArgs ($a | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -WorkDir ($Dir + '\files') -Tag $Tag
    $script:Deadline = (Get-Date).AddSeconds(60)   # the 60 s budget starts once the process exists
    [void](Wait-Window $app)
    Start-Sleep -Milliseconds 1500   # let first frames / background parse settle
    Set-AppFocus $app
    $app
}

# Hash of the bottom strip (status bar) or any pixel region of our window: used to
# detect "the screen changed after my keystroke" and measure repaint latency.
function Get-RegionHash {
    # Default: bottom strip (status bar). With -Logical x,y,w,h: that region (logical px from the outer top-left).
    param($App, [int]$StripPx = 50, [double[]]$Logical = $null)
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($App.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($App.Hwnd, [ref]$r) }
    if ($null -ne $Logical) {
        $s = [W32]::GetDpiForWindow($App.Hwnd) / 96.0
        $sx = $r.Left + [int]($Logical[0] * $s); $sy = $r.Top + [int]($Logical[1] * $s)
        $w = [int]($Logical[2] * $s); $h = [int]($Logical[3] * $s)
    } else {
        $w = $r.Right - $r.Left; $h = $StripPx; $sx = $r.Left; $sy = $r.Bottom - $StripPx - 2
    }
    $bmp = New-Object Drawing.Bitmap($w, $h)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($sx, $sy, 0, 0, (New-Object Drawing.Size($w, $h)))
    $g.Dispose()
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Bmp); $bmp.Dispose()
    $md5 = [Security.Cryptography.MD5]::Create()
    $h = [BitConverter]::ToString($md5.ComputeHash($ms.ToArray()))
    $ms.Dispose(); $h
}

function Get-SessionText { param([string]$Dir) $f = Join-Path $Dir 'settings\session.toml'; if (Test-Path $f) { Get-Content -Raw $f } else { '' } }

function Get-ColdStartMs {
    param($App)
    $m = [regex]::Match((Read-Stderr $App), 'cold start: (\d+) ms')
    if ($m.Success) { [int]$m.Groups[1].Value } else { $null }
}
