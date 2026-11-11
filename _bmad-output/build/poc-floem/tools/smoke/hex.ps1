<#
.SYNOPSIS
  Hex editor on a small binary file, driven through the TEST-INPUT channel (no SendInput, no foreground):
  enter hex mode (palette command 'Hex: Go to Offset...' switches it on), grid + ASCII column, click / shift+click /
  arrow selection, data inspector (ints, floats, endianness), Goto (0x / decimal / relative), nibble edit + undo/redo,
  regions (add, rename, colour, go, delete), Copy as ... (clipboard read back), hashes vs Get-FileHash, Save (bytes on disk),
  focus rules (typing in the goto / region fields never edits bytes), leaving hex mode.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\hex.ps1 -Exe D:\travail\_t-verify\fast\brouillon.exe [-Tag x-]
  The exe must be built with --features test-input. A TEMPORARY --settings-dir is used, never the owner's settings.
  NOTE: 'Hex: Copy as ...' writes the real Windows clipboard (the app has no other clipboard); the script restores nothing.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-hex'), [string]$Tag = '', [switch]$Explore, [switch]$Only)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }

# The fixture: bytes 0-7 = 01..08, 8-15 = "BROUILLO", 16-19 = 00 00 80 3F (float 1.0 LE), 20-23 = FF FF FF FF,
# then (i*7) & 0xFF up to 256 bytes. Known values: u8=1, u16 LE=513 / BE=258, u32 LE=67305985 / BE=16909060.
function New-Fixture([string]$path) {
    $b = New-Object byte[] 256
    for ($i = 0; $i -lt 256; $i++) { $b[$i] = ($i * 7) -band 0xFF }
    1..8 | ForEach-Object { $b[$_ - 1] = $_ }
    [Text.Encoding]::ASCII.GetBytes('BROUILLO').CopyTo($b, 8)
    @(0x00, 0x00, 0x80, 0x3F, 0xFF, 0xFF, 0xFF, 0xFF) | ForEach-Object -Begin { $k = 16 } -Process { $b[$k++] = $_ }
    [IO.File]::WriteAllBytes($path, $b)
    ,$b
}


Add-Type -AssemblyName System.Drawing
# Pixel coordinates of the CLIENT area of the screenshot (what the owner sees). Grid geometry measured on a 1.75 scale
# window: text column 0 at x=437.5, 12.6 px per character, first row centre y=209, 31.5 px per row.
function Scale($app) { [W32]::GetDpiForWindow($app.Hwnd) / 96.0 }
function Px($app, [double]$x, [double]$y, [string]$mods = '', [int]$count = 1) {
    $r = @{ op = 'click'; x = ($x / 1.75); y = ($y / 1.75); count = $count }
    if ($mods) { $r['mods'] = $mods }
    [void](Invoke-TI $app $r)
    Start-Sleep -Milliseconds 200
}
function ByteX($b, [bool]$ascii = $false) {
    if ($ascii) { return 437.5 + (60 + $b + 0.5) * 12.6 }
    $col = 10 + 3 * $b + $(if ($b -ge 8) { 1 } else { 0 })
    437.5 + ($col + 1) * 12.6
}
function ByteY($row) { 209 + 31.5 * $row }
function Click-Byte($app, [int]$i, [switch]$Shift, [switch]$Ascii) {
    $x = ByteX ($i % 16) $Ascii.IsPresent; $y = ByteY ([math]::Floor($i / 16))
    Px $app $x $y $(if ($Shift) { 'shift' } else { '' })
}

# Strips: crops of the hex status line (and optionally the inspector) kept in one composite PNG so a few reads cover many checks.
$script:Strips = New-Object System.Collections.ArrayList
function Strip($app, [string]$label, [switch]$Top, [switch]$Panel) {
    $tmp = Join-Path (Get-SmokeOut) '_strip.png'
    [void](Invoke-TI $app @{ op = 'screenshot'; path = $tmp; settle_ms = 250 })
    $bmp = [Drawing.Bitmap]::FromFile($tmp)
    $parts = @(New-Object Drawing.Rectangle(420, 1186, 1000, 44))
    if ($Panel) { $parts = @((New-Object Drawing.Rectangle(1400, 200, 525, 160))) + $parts }
    if ($Top) { $parts = @((New-Object Drawing.Rectangle(420, 140, 1000, 80))) + $parts }
    $h = ($parts | Measure-Object -Property Height -Sum).Sum
    $out = New-Object Drawing.Bitmap(1000, $h)
    $g = [Drawing.Graphics]::FromImage($out); $y = 0
    foreach ($r in $parts) { $g.DrawImage($bmp, (New-Object Drawing.Rectangle(0, $y, $r.Width, $r.Height)), $r, [Drawing.GraphicsUnit]::Pixel); $y += $r.Height }
    $g.Dispose(); $bmp.Dispose()
    [void]$script:Strips.Add(@($label, $out))
}
function Save-Strips([string]$name) {
    $total = 0; foreach ($e in $script:Strips) { $total += $e[1].Height + 18 }
    $out = New-Object Drawing.Bitmap(1000, $total)
    $g = [Drawing.Graphics]::FromImage($out); $g.Clear([Drawing.Color]::Black)
    $f = New-Object Drawing.Font('Consolas', 11); $y = 0
    foreach ($e in $script:Strips) {
        $g.DrawString($e[0], $f, [Drawing.Brushes]::Yellow, 2, $y)
        $g.DrawImage($e[1], 0, $y + 18); $y += $e[1].Height + 18
        $e[1].Dispose()
    }
    $g.Dispose(); $script:Strips.Clear()
    $path = Join-Path (Get-SmokeOut) "$Tag$name.png"; $out.Save($path); $out.Dispose(); $path
}
function Open-Palette($app, [string]$cmd) {
    Send-Chord $app 'ctrl+k' 200
    $null = Wait-State $app { param($s) $s.state.palette_open }
    Send-Text $app $cmd
    Start-Sleep -Milliseconds 250
    Send-Chord $app 'enter' 400
}
function Get-PixelAt($app, [int]$x, [int]$y) {
    $tmp = Join-Path (Get-SmokeOut) '_px.png'
    [void](Invoke-TI $app @{ op = 'screenshot'; path = $tmp; settle_ms = 200 })
    $bmp = [Drawing.Bitmap]::FromFile($tmp); $c = $bmp.GetPixel($x, $y); $bmp.Dispose()
    '{0:X2}{1:X2}{2:X2}' -f $c.R, $c.G, $c.B
}
function Get-Clip { (Get-Clipboard -Raw) }
function Hexs([byte[]]$b) { ($b | ForEach-Object { $_.ToString('X2') }) -join '' }


Invoke-Scenario 'hex-esc' 'Hex: Esc in the goto field' {
    $d = New-TestEnv $DataRoot 'hex3'
    $null = New-Fixture "$d\files\data.bin"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\data.bin") -Tag 'hex3'
    Send-Chord $app 'ctrl+shift+h' 800
    Click-Byte $app 40
    Send-Chord $app 'ctrl+g' 300; Send-Text $app 'abcdef'
    Shot $app 'hexesc-1-typed'
    Send-Chord $app 'escape' 400
    Shot $app 'hexesc-2-after-esc'
    Send-Chord $app 'right' 300
    Shot $app 'hexesc-3-after-right'
    $s = Get-State $app 60
    ($s.lines | Select-Object -Last 8) -join ' // '
}
if ($Only) { $script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host; return }
Invoke-Scenario 'hex-main' 'Hex: select, inspect, goto, edit, copy, hash, save' {
    $d = New-TestEnv $DataRoot 'hex'
    $bytes = New-Fixture "$d\files\data.bin"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\data.bin") -Tag 'hex'
    Send-Chord $app 'ctrl+shift+h' 800
    Strip $app 'enter hex mode: cursor at 0'
    # --- selection: click, shift+click, arrows, Esc
    Click-Byte $app 5
    Strip $app 'click byte 5'
    Click-Byte $app 12 -Shift
    Strip $app 'shift+click byte 12 -> Sel 0x5-0xC (8 B)'
    Send-Chord $app 'escape' 200
    Strip $app 'Esc collapses'
    Click-Byte $app 3 -Ascii
    Strip $app 'click ASCII column byte 3'
    Send-Chord $app 'right' 100; Send-Chord $app 'right' 100; Send-Chord $app 'shift+down' 150
    Strip $app 'right x2 then shift+down: cursor 0x15'
    Save-Strips 'hex-2-selection-strips' | Out-Null
    Shot $app 'hex-2-selection'
    # --- inspector: 4 bytes at 0, LE then BE
    Click-Byte $app 0
    Send-Chord $app 'shift+right' 80; Send-Chord $app 'shift+right' 80; Send-Chord $app 'shift+right' 150
    Shot $app 'hex-3-inspector-le'
    Open-Palette $app 'Hex: Toggle Little/Big Endian'
    Shot $app 'hex-4-inspector-be'
    Open-Palette $app 'Hex: Toggle Little/Big Endian'
    # --- float at 0x10 (00 00 80 3F = 1.0)
    Click-Byte $app 16
    Send-Chord $app 'shift+right' 80; Send-Chord $app 'shift+right' 80; Send-Chord $app 'shift+right' 150
    Shot $app 'hex-5-float'
    # --- goto: hex, decimal, relative, end-relative, error
    foreach ($g in @(@('0x40', 'goto 0x40'), @('100', 'goto 100 -> 0x64'), @('+0x10', 'goto +0x10 -> 0x74'), @('end-8', 'goto end-8 -> 0xF8'), @('0xZZ', 'goto 0xZZ -> error'), @('9999', 'goto 9999 past the end'))) {
        Send-Chord $app 'ctrl+g' 250
        Send-Text $app $g[0]
        Send-Chord $app 'enter' 250
        Strip $app $g[1] -Top
        if ($g[0] -in @('0xZZ', '9999')) { Send-Chord $app 'escape' 200 }
    }
    Save-Strips 'hex-6-goto' | Out-Null
    'part 1'
}

Invoke-Scenario 'hex-edit' 'Hex: nibble edit, undo/redo, save, regions, copy, hash' {
    $d = New-TestEnv $DataRoot 'hex2'
    $bytes = New-Fixture "$d\files\data.bin"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\data.bin") -Tag 'hex2'
    Send-Chord $app 'ctrl+shift+h' 800
    # --- nibble edit at 0x20 (E0): type A then B
    Click-Byte $app 32
    Send-Chord $app 'a' 120
    Strip $app 'typed A: low digit pending'
    Send-Chord $app 'b' 150
    Strip $app 'typed B: byte 0x20 = AB, dirty'
    Shot $app 'hex-7-edited'
    Send-Chord $app 'ctrl+z' 200
    Strip $app 'undo'
    Shot $app 'hex-8-undone'
    Send-Chord $app 'ctrl+y' 200
    Strip $app 'redo'
    Send-Chord $app 'ctrl+s' 500
    Strip $app 'saved'
    $disk = [IO.File]::ReadAllBytes("$d\files\data.bin")
    if ($disk.Length -ne 256 -or $disk[32] -ne 0xAB) { throw "after Save byte 0x20 is $('{0:X2}' -f $disk[32]) (len $($disk.Length)), expected AB / 256" }
    for ($i = 0; $i -lt 256; $i++) { if ($i -ne 32 -and $disk[$i] -ne $bytes[$i]) { throw "unexpected change at $i" } }
    Save-Strips 'hex-9-edit-strips' | Out-Null
    # --- regions: select 8..15, panel on, name it, add
    Open-Palette $app 'Hex: Toggle Data Inspector'
    Open-Palette $app 'Hex: Toggle Regions Panel'
    Click-Byte $app 8
    Click-Byte $app 15 -Shift
    Shot $app 'hex-10-regions-panel'
    # focus rule: typing in the region-name field must not edit the bytes
    $before = $null
    Px $app 1520 265
    Send-Text $app 'magic'
    Strip $app 'typed magic in the region name field: no byte edits expected (status unchanged)'
    Px $app 1718 264
    Shot $app 'hex-11-region-added'
    # --- Copy as ... (select 0..3), read the clipboard back
    Click-Byte $app 0
    Click-Byte $app 3 -Shift
    $exp = @{
        'Hex: Copy as hex string' = '01020304'
        'Hex: Copy as spaced hex' = '01 02 03 04'
        'Hex: Copy as Base64'     = [Convert]::ToBase64String([byte[]](1, 2, 3, 4))
    }
    foreach ($k in $exp.Keys) {
        Set-Clipboard -Value 'SENTINEL'
        Open-Palette $app $k
        Start-Sleep -Milliseconds 200
        $c = (Get-Clip).Trim()
        if ($c -ne $exp[$k]) { throw "$k put '$c' on the clipboard, expected '$($exp[$k])'" }
    }
    $dump = @()
    foreach ($k in @('Hex: Copy as C array', 'Hex: Copy as Rust array', 'Hex: Copy as Python bytes', 'Hex: Copy as ASCII text')) {
        Set-Clipboard -Value 'SENTINEL'
        Open-Palette $app $k
        Start-Sleep -Milliseconds 200
        $dump += "$k => " + ((Get-Clip) -replace "`r?`n", '
')
    }
    [IO.File]::WriteAllLines((Join-Path (Get-SmokeOut) "$($Tag)hex-12-copy-formats.txt"), $dump)
    # --- hash of bytes 0..15 vs known values
    Click-Byte $app 0
    Click-Byte $app 15 -Shift
    Open-Palette $app 'Hex: Hash Selection'
    Start-Sleep -Milliseconds 1200
    Shot $app 'hex-13-hash'
    # click each checksum row: the value goes to the clipboard, compare with the independent values
    $slice = $bytes[0..15]
    $tmpf = Join-Path $d 'slice.bin'; [IO.File]::WriteAllBytes($tmpf, [byte[]]$slice)
    $want = @(
        @(1102, '8ba3da3d'),   # CRC-32 of bytes 0..15 (python zlib.crc32)
        @(1126, (Get-FileHash $tmpf -Algorithm MD5).Hash.ToLower()),
        @(1150, (Get-FileHash $tmpf -Algorithm SHA1).Hash.ToLower()),
        @(1171, (Get-FileHash $tmpf -Algorithm SHA256).Hash.ToLower())
    )
    foreach ($w in $want) {
        Set-Clipboard -Value 'SENTINEL'
        Px $app 800 $w[0]
        $c = (Get-Clip).Trim().ToLower()
        if ($c -ne $w[1]) { throw "hash row at y=$($w[0]) copied '$c', expected '$($w[1])'" }
    }
    # --- regions: colour, rename, go, delete
    $sw0 = Get-PixelAt $app 1426 319
    Px $app 1721 319
    $sw1 = Get-PixelAt $app 1426 319
    if ($sw0 -eq $sw1) { throw "Colour did not change the swatch ($sw0)" }
    Px $app 1480 308          # pick the region: its name goes to the field
    Px $app 1520 265; Send-Chord $app 'ctrl+a' 100; Send-Text $app 'renamed'
    Px $app 1824 264          # Rename
    Strip $app 'region renamed, colour changed' -Panel
    Click-Byte $app 100
    Px $app 1618 319          # Go
    Strip $app 'Go: cursor back on 0x8' -Panel
    Px $app 1849 319          # Delete
    Strip $app 'Delete: list empty' -Panel
    Save-Strips 'hex-14-regions-ops' | Out-Null
    # --- focus rules: goto field swallows typing, Esc closes it
    Click-Byte $app 40
    Send-Chord $app 'ctrl+g' 250; Send-Text $app 'abcdef'
    Strip $app 'typed abcdef in the goto field' -Top
    Send-Chord $app 'escape' 250
    Strip $app 'Esc closed the goto field; no byte edits (no unsaved marker)' -Top
    Send-Chord $app 'right' 150
    Strip $app 'arrow works again after Esc' -Top
    $disk = [IO.File]::ReadAllBytes("$d\files\data.bin")
    if ($disk[32] -ne 0xAB) { throw 'disk changed unexpectedly' }
    Save-Strips 'hex-15-focus' | Out-Null
    # --- leave hex mode, come back with an unsaved edit, then quit with it
    Send-Chord $app 'a' 100; Send-Chord $app 'b' 150
    Strip $app 'dirty before leaving'
    Send-Chord $app 'ctrl+shift+h' 600
    Shot $app 'hex-16-left-hex-mode'
    Send-Chord $app 'ctrl+shift+h' 800
    Strip $app 'back in hex mode: the unsaved edit must still be there'
    Save-Strips 'hex-17-reenter' | Out-Null
    try { [void](Invoke-TI $app @{ op = 'quit' }) } catch { }
    $gone = $app.Proc.WaitForExit(4000)
    "hashes OK (clipboard vs Get-FileHash/zlib); quit op (test-only, bypasses the guard) exited = $gone"
}

# ---- quit with unsaved byte edits: the app must ask (Cancel keeps it open, Don't save quits, Save writes then quits).
Add-Type -TypeDefinition @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class QuitDlg {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  // Visible top-level window of the process whose title contains `part` (the native Save / Don't save / Cancel box).
  public static IntPtr Find(uint pid, string part) {
    IntPtr found = IntPtr.Zero;
    EnumWindows(delegate (IntPtr h, IntPtr l) {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != pid || !IsWindowVisible(h)) return true;
      var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
      if (sb.ToString().Contains(part)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@
function Wait-QuitDialog($app, [int]$ms = 6000) {
    $end = (Get-Date).AddMilliseconds($ms)
    while ((Get-Date) -lt $end) {
        $h = [QuitDlg]::Find([uint32]$app.Proc.Id, 'Quit?')
        if ($h -ne [IntPtr]::Zero) { return $h }
        Start-Sleep -Milliseconds 100
    }
    throw 'no quit confirmation dialog appeared'
}
# IDYES=6 (Save), IDNO=7 (Don't save), IDCANCEL=2. WM_COMMAND reaches both classic and task dialogs.
function Answer-Dialog($h, [int]$id) { [void][QuitDlg]::PostMessage($h, 0x111, [IntPtr]$id, [IntPtr]::Zero) }
function Dirty-Byte($app) {   # enter hex mode, type AB at 0x20 (not saved)
    Send-Chord $app 'ctrl+shift+h' 800
    Click-Byte $app 32
    Send-Chord $app 'a' 120; Send-Chord $app 'b' 200
}

foreach ($how in @('ctrl+q', 'wm_close')) {
    Invoke-Scenario "hex-quit-$how" "Hex: quit via $how with unsaved bytes" {
        $d = New-TestEnv $DataRoot "hexq-$how"
        $bytes = New-Fixture "$d\files\data.bin"
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\data.bin") -Tag "hexq-$how"
        Dirty-Byte $app
        # the modal box keeps the key handler busy, so the channel answers 'busy' after 5 s (tolerated for ctrl+q)
        $trigger = { if ($how -eq 'ctrl+q') { try { Send-Chord $app 'ctrl+q' 100 } catch { if ($_ -notmatch 'did not answer') { throw }; $script:Deadline = $script:Deadline.AddSeconds(6) } } else { [void][QuitDlg]::PostMessage($app.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) } }
        # 1) Cancel: the app must stay open, edit kept, disk untouched
        & $trigger
        $h = Wait-QuitDialog $app
        Answer-Dialog $h 2
        Start-Sleep -Milliseconds 800
        if ($app.Proc.HasExited) { throw 'Cancel quit the app' }
        [void](Invoke-TI $app @{ op = 'ping' })
        Shot $app "hexquit-$how-1-after-cancel"
        if ([IO.File]::ReadAllBytes("$d\files\data.bin")[32] -eq 0xAB) { throw 'disk changed by Cancel' }
        # 2) Save: the bytes are written, then the app exits
        & $trigger
        $h = Wait-QuitDialog $app
        Answer-Dialog $h 6
        if (-not $app.Proc.WaitForExit(8000)) { throw 'Save did not quit the app' }
        $disk = [IO.File]::ReadAllBytes("$d\files\data.bin")
        if ($disk[32] -ne 0xAB) { throw "Save did not write the byte: $('{0:X2}' -f $disk[32])" }
        # 3) Don't save: relaunch, dirty again, quit discards and exits
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\data.bin") -Tag "hexq2-$how"
        Dirty-Byte $app
        & $trigger
        $h = Wait-QuitDialog $app
        Answer-Dialog $h 7
        if (-not $app.Proc.WaitForExit(8000)) { throw "Don't save did not quit the app" }
        "cancel kept the app open, Save wrote 0xAB then exited, Don't save exited ($how)"
    }
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
