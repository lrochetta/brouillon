# Brouillon GUI smoke harness - TEST-INPUT backend (PowerShell 5.1, no external dependency).
#
# Drop-in replacement for lib.ps1: change the dot-source line of a scenario from
#     . (Join-Path $PSScriptRoot 'lib.ps1')
# to
#     . (Join-Path $PSScriptRoot 'testinput.ps1')
# It loads lib.ps1 (process handling, scenarios, New-TestEnv ...) and then REPLACES the functions that used the real
# keyboard, mouse, foreground and screen: Send-Chord, Send-Text, Send-Click, Send-Drag, Save-Shot, Get-RegionHash,
# Assert-Focus, Set-AppFocus, Launch-Brouillon. Nothing here calls SendInput, SetCursorPos, SetForegroundWindow or
# CopyFromScreen: the owner keeps using the PC while a scenario runs.
#
# REQUIREMENTS: -Exe must be a DEV build with the `test-input` feature (cargo build --features test-input). A normal
# exe has no such channel: Launch-Brouillon then fails with TESTINPUT_NOT_AVAILABLE (it never falls back to real input).
# SECURITY: a random per-run token travels in the environment of the child process only; the pipe name is random too.
# Threat model: docs/TESTING.md.

. (Join-Path $PSScriptRoot 'lib.ps1')

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)] public struct TIPOINT { public int X, Y; }
public static class TI32 {
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref TIPOINT p);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(ref TIPOINT p);
}
"@ -ErrorAction SilentlyContinue

# ---------------------------------------------------------------- protocol

function New-TIToken {
    $b = New-Object byte[] 24
    $rng = [Security.Cryptography.RNGCryptoServiceProvider]::new()
    $rng.GetBytes($b); $rng.Dispose()
    ([Convert]::ToBase64String($b)).Replace('+', 'a').Replace('/', 'b').Replace('=', 'c')
}

function Connect-TI {
    param($App, [string]$Pipe, [string]$Token, [int]$TimeoutMs = 20000)
    $c = New-Object IO.Pipes.NamedPipeClientStream('.', $Pipe, [IO.Pipes.PipeDirection]::InOut)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        if ($App.Proc.HasExited) { throw "PROCESS_EXITED before the test channel was up (exit code $($App.Proc.ExitCode)); is this a build with --features test-input?" }
        try { $c.Connect(500); break } catch [TimeoutException] { } catch { Start-Sleep -Milliseconds 100 }
        if ($sw.ElapsedMilliseconds -gt $TimeoutMs) { throw "TESTINPUT_NOT_AVAILABLE: no pipe '$Pipe' within $TimeoutMs ms (exe built without --features test-input?)" }
    }
    $enc = New-Object Text.UTF8Encoding($false)
    $w = New-Object IO.StreamWriter($c, $enc); $w.AutoFlush = $true; $w.NewLine = "`n"
    $r = New-Object IO.StreamReader($c, $enc)
    $App | Add-Member -NotePropertyName TI -NotePropertyValue ([pscustomobject]@{ Client = $c; W = $w; R = $r; Seq = 0 }) -Force
    $w.WriteLine((@{ auth = $Token } | ConvertTo-Json -Compress))
    $hello = Read-TILine $App.TI 5000
    if (-not $hello.ok) { throw "TESTINPUT_AUTH_REFUSED: $($hello.error)" }
}

function Read-TILine($ti, [int]$TimeoutMs) {
    $t = $ti.R.ReadLineAsync()
    if (-not $t.Wait($TimeoutMs)) { throw "TESTINPUT_TIMEOUT: no reply within $TimeoutMs ms (app hung?)" }
    if ($null -eq $t.Result) { throw 'TESTINPUT_CLOSED: the app closed the channel' }
    $t.Result | ConvertFrom-Json
}

# Invoke-TI $App @{ op = 'key'; chord = 'ctrl+k' }  -> reply object (throws when ok = false)
function Invoke-TI {
    param($App, [hashtable]$Req, [int]$TimeoutMs = 15000)
    Test-Deadline
    if ($App.Proc.HasExited) { throw 'PROCESS_EXITED' }
    $App.TI.Seq++
    $Req['id'] = $App.TI.Seq
    $App.TI.W.WriteLine(($Req | ConvertTo-Json -Compress -Depth 5))
    $rep = Read-TILine $App.TI $TimeoutMs
    if (-not $rep.ok) { throw "TESTINPUT_ERROR($($Req.op)): $($rep.error)" }
    $rep
}

# ---------------------------------------------------------------- launch (no focus, no foreground)

function Launch-Brouillon {
    param([string]$Exe, [string]$Dir, [string[]]$Files = @(), [string[]]$Extra = @('--new-window'), [string]$Tag, [switch]$Visible, [switch]$Gpu)
    $pipe = 'brouillon-ti-' + [Guid]::NewGuid().ToString('N').Substring(0, 12)
    $token = New-TIToken
    $env:BROUILLON_TEST_INPUT = $pipe
    $env:BROUILLON_TEST_INPUT_TOKEN = $token
    # Not activated, bottom of the z-order, then made alpha 1/255 + click-through by the app: nothing appears on the owner's screen and the foreground is never taken.
    if ($Visible) { Remove-Item Env:\BROUILLON_TEST_INPUT_BACKGROUND -ErrorAction SilentlyContinue } else { $env:BROUILLON_TEST_INPUT_BACKGROUND = '1' }
    # PrintWindow cannot read a GPU (DXGI flip) swap chain: it returns a blank image. The CPU rasteriser (softbuffer)
    # is captured correctly, so scenarios use it unless -Gpu is asked for (then Save-Shot will be blank).
    if (-not $Gpu -and -not $env:FLOEM_FORCE_TINY_SKIA) { $env:FLOEM_FORCE_TINY_SKIA = '1' }
    try {
        $a = @('--settings-dir', ($Dir + '\settings')) + $Extra + $Files
        $app = Start-App -Exe $Exe -AppArgs ($a | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -WorkDir ($Dir + '\files') -Tag $Tag
    } finally {
        # the child has its copy; the token must not leak into the next process this script starts
        Remove-Item Env:\BROUILLON_TEST_INPUT, Env:\BROUILLON_TEST_INPUT_TOKEN -ErrorAction SilentlyContinue
    }
    $script:Deadline = (Get-Date).AddSeconds(60)
    Connect-TI $app $pipe $token
    [void](Wait-Window $app)
    Start-Sleep -Milliseconds 1500   # first frames / background parse
    [void](Invoke-TI $app @{ op = 'ping' })
    $app
}

# Nothing to focus: kept so scenarios that call them keep working.
function Set-AppFocus { param($App) }
function Assert-Focus { param($App) Test-Deadline; if ($App.Proc.HasExited) { throw 'PROCESS_EXITED' } }

# ---------------------------------------------------------------- input

function Send-Chord {
    param($App, [string]$Chord, [int]$PauseMs = 120)
    [void](Invoke-TI $App @{ op = 'key'; chord = $Chord })
    Start-Sleep -Milliseconds $PauseMs
}

# Hold modifiers ("ctrl") down, tap keys with them held, release: Send-Hold $app "ctrl"; Send-Tap $app "ctrl+tab"; Send-Release $app "ctrl"
function Send-Hold    { param($App, [string]$Mods) [void](Invoke-TI $App @{ op = "hold"; mods = $Mods }); Start-Sleep -Milliseconds 80 }
function Send-Release { param($App, [string]$Mods) [void](Invoke-TI $App @{ op = "release"; mods = $Mods }); Start-Sleep -Milliseconds 200 }
function Send-Tap     { param($App, [string]$Chord, [int]$PauseMs = 200) [void](Invoke-TI $App @{ op = "tap"; chord = $Chord }); Start-Sleep -Milliseconds $PauseMs }

function Send-Text {
    param($App, [string]$Text, [int]$CharDelayMs = 12)
    [void](Invoke-TI $App @{ op = 'text'; text = $Text })
    Start-Sleep -Milliseconds 80
}

# lib.ps1 coordinates are LOGICAL px from the top-left of the OUTER window frame (title bar included). The channel
# works in client coordinates, so the frame offset (title bar, borders) is subtracted here.
function ConvertTo-ClientXY {
    param($App, [double]$LX, [double]$LY)
    $r = New-Object RECT
    $hr = [W32]::DwmGetWindowAttribute($App.Hwnd, 9, [ref]$r, 16)
    if ($hr -ne 0) { [void][W32]::GetWindowRect($App.Hwnd, [ref]$r) }
    $p = New-Object TIPOINT
    [void][TI32]::ClientToScreen($App.Hwnd, [ref]$p)
    $scale = [W32]::GetDpiForWindow($App.Hwnd) / 96.0
    @((($LX - ($p.X - $r.Left) / $scale)), (($LY - ($p.Y - $r.Top) / $scale)))
}

function Send-Click {
    param($App, [double]$LX, [double]$LY, [switch]$Double, [string]$Mods = '')
    $c = ConvertTo-ClientXY $App $LX $LY
    $n = 1; if ($Double) { $n = 2 }
    $req = @{ op = 'click'; x = $c[0]; y = $c[1]; count = $n }
    if ($Mods) { $req.mods = $Mods }
    [void](Invoke-TI $App $req)
    Start-Sleep -Milliseconds 150
}

function Send-Drag {
    param($App, [double]$X1, [double]$Y1, [double]$X2, [double]$Y2, [int]$Steps = 12)
    $a = ConvertTo-ClientXY $App $X1 $Y1; $b = ConvertTo-ClientXY $App $X2 $Y2
    [void](Invoke-TI $App @{ op = 'drag'; x1 = $a[0]; y1 = $a[1]; x2 = $b[0]; y2 = $b[1]; steps = $Steps })
    Start-Sleep -Milliseconds 200
}

function Send-Wheel {
    param($App, [double]$LX, [double]$LY, [double]$Dy, [double]$Dx = 0)
    $c = ConvertTo-ClientXY $App $LX $LY
    [void](Invoke-TI $App @{ op = 'wheel'; x = $c[0]; y = $c[1]; dx = $Dx; dy = $Dy })
    Start-Sleep -Milliseconds 120
}

function Set-AppSize {
    param($App, [double]$W, [double]$H)
    [void](Invoke-TI $App @{ op = 'resize'; w = $W; h = $H })
    Start-Sleep -Milliseconds 300
}

# ---------------------------------------------------------------- capture (window content only, any z-order)

# Note: the PNG is the CLIENT area (no title bar / frame), unlike lib.ps1's outer-frame screen copy.
function Save-Shot {
    param($App, [string]$Name, [int]$SettleMs = 500)
    $out = Get-SmokeOut
    $path = Join-Path $out "$Name.png"
    $rep = Invoke-TI $App @{ op = 'screenshot'; path = $path; settle_ms = $SettleMs }
    $path
}

function Get-RegionHash {
    param($App, [int]$StripPx = 50, [double[]]$Logical = $null)
    $tmp = Join-Path (Get-SmokeOut) ("_hash-" + $App.Proc.Id + ".png")
    [void](Invoke-TI $App @{ op = 'screenshot'; path = $tmp; settle_ms = 60 })
    $bmp = [Drawing.Bitmap]::FromFile($tmp)
    try {
        if ($null -ne $Logical) {
            $s = [W32]::GetDpiForWindow($App.Hwnd) / 96.0
            $c = ConvertTo-ClientXY $App $Logical[0] $Logical[1]
            $rect = New-Object Drawing.Rectangle([int]($c[0] * $s), [int]($c[1] * $s), [int]($Logical[2] * $s), [int]($Logical[3] * $s))
        } else {
            $rect = New-Object Drawing.Rectangle(0, ($bmp.Height - $StripPx - 2), $bmp.Width, $StripPx)
        }
        $rect.Intersect((New-Object Drawing.Rectangle(0, 0, $bmp.Width, $bmp.Height)))
        $crop = $bmp.Clone($rect, $bmp.PixelFormat)
        $ms = New-Object IO.MemoryStream
        $crop.Save($ms, [Drawing.Imaging.ImageFormat]::Bmp); $crop.Dispose()
        $h = [BitConverter]::ToString([Security.Cryptography.MD5]::Create().ComputeHash($ms.ToArray()))
        $ms.Dispose(); $h
    } finally { $bmp.Dispose(); Remove-Item $tmp -ErrorAction SilentlyContinue }
}

# ---------------------------------------------------------------- state

function Get-State { param($App, [int]$Lines = 60) Invoke-TI $App @{ op = 'state'; lines = $Lines } }

# Wait-State $App { param($s) $s.state.palette_open }        -> the state object once the predicate is true
# Wait-State $App -Line 'strip=connected'                    -> once a state line (regex) has appeared
function Wait-State {
    param($App, [scriptblock]$Predicate, [string]$Line, [int]$TimeoutMs = 8000)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $s = Get-State $App 400
        if ($Predicate -and (& $Predicate $s)) { return $s }
        if ($Line -and (@($s.lines) | Where-Object { $_ -match $Line })) { return $s }
        if (-not $Predicate -and -not $Line) { return $s }
        if ($sw.ElapsedMilliseconds -gt $TimeoutMs) { throw "WAIT_STATE_TIMEOUT after $TimeoutMs ms (tab=$($s.state.tab_title) caret=$($s.state.caret) palette=$($s.state.palette_open))" }
        Start-Sleep -Milliseconds 100
    }
}
