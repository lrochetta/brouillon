<#
.SYNOPSIS
  Brouillon GUI smoke run (scenarios a..k). PowerShell 5.1, nothing to install.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\run.ps1 -Exe C:\path\brouillon.exe
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\run.ps1 -Build            # cargo build --release first
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\run.ps1 -Exe ... -Only a,d,k

  The app is always launched with a TEMPORARY --settings-dir and synthetic files
  under -DataRoot. Keystrokes/clicks are sent only while the foreground window
  belongs to the launched process (otherwise the scenario is ABORTED(focus)).
  Do not touch keyboard/mouse while it runs (about 6 minutes).
#>
param(
    [string]$Exe,
    [switch]$Build,
    [string[]]$Only,
    [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-smoke'),
    [switch]$KeepData,
    [switch]$SkipBig
)
$ErrorActionPreference = 'Stop'
if ($Only) { $Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ }) }   # powershell -File passes "a,b" as one string
. (Join-Path $PSScriptRoot 'lib.ps1')

$workspace = Resolve-Path (Join-Path $PSScriptRoot '..\..')
if ($Build) {
    Push-Location $workspace
    try { cargo build --release -p brouillon; if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' } } finally { Pop-Location }
    $tgt = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $workspace 'target' }
    $Exe = Join-Path $tgt 'release\brouillon.exe'
}
if (-not $Exe -or -not (Test-Path $Exe)) { throw 'pass -Exe <path to brouillon.exe> or -Build' }
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $DataRoot | Out-Null

function Want([string]$id) { (-not $Only) -or ($Only -contains $id) }
function Wait-Until([scriptblock]$Cond, [int]$Ms = 5000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $Ms) { if (& $Cond) { return $true }; Start-Sleep -Milliseconds 100 }
    $false
}
function Utf8([string]$s) { New-Object Text.UTF8Encoding($false) }
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }
function Assert([bool]$c, [string]$m) { if (-not $c) { throw $m } }
# Layout constants (LOGICAL px from the window's outer top-left; measured on screenshots at 175% DPI / 1100x720 logical window).
$FIND_FIELD = @(723, 161); $REPL_FIELD = @(723, 189); $REPL_ALL = @(968, 189); $EDITOR_TEXT = @(600, 119)   # Find bar: absolute at OVERLAY_TOP 112 (unchanged). EDITOR_TEXT was 143 before f694fd3 removed the 24 px toolbar row (-24)

# ======================================================================= a
if (Want 'a') { Invoke-Scenario 'a' 'open file, type, save, check disk' {
    $d = New-TestEnv $DataRoot 'a'
    Write-Utf8 "$d\files\a.txt" "alpha`nbeta`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'a'
    Save-Shot $app 'a1-opened' | Out-Null
    $textRegion = @(190, 137, 500, 110)   # logical: first text rows of the editor body (left of the autocomplete popup)
    Start-Sleep -Milliseconds 400
    $hBefore = Get-RegionHash $app -Logical $textRegion
    Send-Text $app 'XYZ'
    Start-Sleep -Milliseconds 500
    $hAfter = Get-RegionHash $app -Logical $textRegion
    Save-Shot $app 'a2-typed' | Out-Null
    # Regression guard (stale rows): the editor body MUST repaint after typing.
    Assert ($hBefore -ne $hAfter) 'typed characters never reached the screen (editor body pixels unchanged)'
    Send-Chord $app 'ctrl+s'
    Save-Shot $app 'a3-saved' | Out-Null
    $ok = Wait-Until { (Get-Content -Raw "$d\files\a.txt") -like 'XYZalpha*' } 3000
    $disk = (Get-Content -Raw "$d\files\a.txt")
    Assert $ok "disk content is '$($disk -replace "`n",'\n')' (expected to start with XYZalpha)"
    $m = Get-Mem $app
    "disk='$($disk -replace "`n",'\n')' windowMs=$($app.WindowMs) coldStartMs=$(Get-ColdStartMs $app) ws=$($m.WorkingSetMB)MB"
} }

# ======================================================================= b
if (Want 'b') { Invoke-Scenario 'b' 'Ctrl+N / Ctrl+W tabs, switch tab' {
    $d = New-TestEnv $DataRoot 'b'
    Write-Utf8 "$d\files\a.txt" "alpha`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'b'
    Send-Chord $app 'ctrl+n'; Send-Chord $app 'ctrl+n'
    Save-Shot $app 'b1-three-tabs' | Out-Null
    $n3 = ([regex]::Matches((Get-SessionText $d), '\[\[tabs\]\]')).Count
    Send-Chord $app 'ctrl+pgup'
    Save-Shot $app 'b2-switched' | Out-Null
    $act = [regex]::Match((Get-SessionText $d), 'active_idx = (\d+)').Groups[1].Value
    Send-Chord $app 'ctrl+w'
    Save-Shot $app 'b3-closed-one' | Out-Null
    $n2 = ([regex]::Matches((Get-SessionText $d), '\[\[tabs\]\]')).Count
    # NOTE: empty scratch tabs are not persisted by the session, so the session count is only a lower bound.
    "session tabs after 2xCtrl+N=$n3, active_idx after Ctrl+PgUp=$act, tabs after Ctrl+W=$n2 (see b1..b3 screenshots)"
} }

# ======================================================================= c
if (Want 'c') { Invoke-Scenario 'c' 'find, replace all, single Ctrl+Z undoes all' {
    $d = New-TestEnv $DataRoot 'c'
    $orig = "foo one`nfoo two`nbar foo`n"
    Write-Utf8 "$d\files\c.txt" $orig
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\c.txt") -Tag 'c'
    Send-Chord $app 'ctrl+h'
    Send-Click $app $FIND_FIELD[0] $FIND_FIELD[1]; Send-Text $app 'foo'
    Send-Click $app $REPL_FIELD[0] $REPL_FIELD[1]; Send-Text $app 'qux'
    Save-Shot $app 'c1-find-replace-filled' | Out-Null
    Send-Click $app $REPL_ALL[0] $REPL_ALL[1]
    Save-Shot $app 'c2-replaced' | Out-Null
    Send-Chord $app 'esc'
    Send-Click $app $EDITOR_TEXT[0] $EDITOR_TEXT[1]
    Send-Chord $app 'ctrl+s'
    Assert (Wait-Until { (Get-Content -Raw "$d\files\c.txt") -like 'qux one*' } 3000) "Replace All did not reach disk: '$((Get-Content -Raw "$d\files\c.txt") -replace "`n",'\n')'"
    $after = (Get-Content -Raw "$d\files\c.txt")
    Assert ($after -eq "qux one`nqux two`nbar qux`n") "unexpected replace result '$($after -replace "`n",'\n')'"
    Send-Chord $app 'ctrl+z'
    Save-Shot $app 'c3-after-undo' | Out-Null
    Send-Chord $app 'ctrl+s'
    $undone = Wait-Until { (Get-Content -Raw "$d\files\c.txt") -eq $orig } 3000
    $now = (Get-Content -Raw "$d\files\c.txt")
    Assert $undone "ONE Ctrl+Z did not restore the original; disk='$($now -replace "`n",'\n')'"
    'Replace All (3 hits) then one Ctrl+Z restored original text (verified on disk after save)'
} }

# ======================================================================= d
if (Want 'd') { Invoke-Scenario 'd' 'session: tabs, active file, cursor, scratch text restored after close/reopen' {
    $d = New-TestEnv $DataRoot 'd'
    $lines = (1..120 | ForEach-Object { "line $_ of file one" }) -join "`n"
    Write-Utf8 "$d\files\one.txt" ($lines + "`n")
    Write-Utf8 "$d\files\two.txt" ((1..120 | ForEach-Object { "second $_" }) -join "`n")
    Write-Utf8 "$d\files\three.txt" "three`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\one.txt", "$d\files\two.txt", "$d\files\three.txt") -Tag 'd1'
    Save-Shot $app 'd1-three-opened' | Out-Null
    # go to line 40 col via Ctrl+G in the active tab, then move to another tab
    Send-Chord $app 'ctrl+g'; Send-Text $app "40`n"
    Send-Text $app 'MARK'     # makes the active file dirty (unsaved text)
    Save-Shot $app 'd2-line40-dirty' | Out-Null
    Send-Chord $app 'ctrl+n'; Send-Text $app 'scratch-text-survives'
    Save-Shot $app 'd3-scratch' | Out-Null
    Send-Chord $app 'ctrl+pgup'    # back to the file tab
    $before = Get-SessionText $d
    $r = Stop-AppGraceful $app
    $sess = Get-SessionText $d
    $err1 = Read-Stderr $app
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'd2'
    Save-Shot $app2 'd5-reopened' | Out-Null
    $err2 = Read-Stderr $app2
    $restored = [regex]::Match($err2, 'restored (\d+) tab').Groups[1].Value
    $rec = [regex]::Match($err2, '(\d+) document\(s\) recovered').Groups[1].Value
    $curs = ([regex]::Matches($sess, 'cursor_line = (\d+)\r?\ncursor_col = (\d+)\r?\nscroll_line = (\d+)') | ForEach-Object { "$($_.Groups[1].Value):$($_.Groups[2].Value)/scroll$($_.Groups[3].Value)" }) -join ' '
    $detail = "close=$r; restoredTabs=$restored; recoveredDocs=$rec; session cursors=[$curs]; (look at d5-reopened.png for active tab, scratch text, dirty MARK)"
    Assert ($restored -ne '' -and [int]$restored -ge 3) "tabs not restored: $detail"
    $detail
} }

# ======================================================================= e
if (Want 'e') { Invoke-Scenario 'e' 'command palette' {
    $d = New-TestEnv $DataRoot 'e'
    Write-Utf8 "$d\files\e.txt" "palette`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\e.txt") -Tag 'e'
    Send-Chord $app 'ctrl+k'
    Save-Shot $app 'e1-palette-open' | Out-Null
    Send-Text $app 'upper'
    Save-Shot $app 'e2-palette-filtered' | Out-Null
    Send-Chord $app 'esc'
    $err = Read-Stderr $app
    Assert ($err -match '\[palette\] open') 'palette never reported open'
    Send-Chord $app 'ctrl+k'; Send-Text $app 'save'; Send-Chord $app 'enter'
    Save-Shot $app 'e3-palette-executed' | Out-Null
    'palette opens (stderr), filters, executes a command: inspect e1..e3'
} }

# ======================================================================= f
if (Want 'f') { Invoke-Scenario 'f' 'Find in Files on a test folder + F4' {
    $d = New-TestEnv $DataRoot 'f'
    New-Item -ItemType Directory -Force "$d\files\proj\sub" | Out-Null
    Write-Utf8 "$d\files\proj\a1.txt" "nothing here`nsecond line`n"
    Write-Utf8 "$d\files\proj\b2.txt" "x`ny`nthe NEEDLE_XYZ is here`nz`n"
    Write-Utf8 "$d\files\proj\sub\c3.txt" "NEEDLE_XYZ at the start`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\proj\a1.txt") -Tag 'f'
    Send-Chord $app 'ctrl+f'
    Send-Click $app $FIND_FIELD[0] $FIND_FIELD[1]; Send-Text $app 'NEEDLE_XYZ'
    Send-Chord $app 'ctrl+shift+f'
    Save-Shot $app 'f1-results' -SettleMs 1500 | Out-Null
    Send-Chord $app 'f4'
    Save-Shot $app 'f2-after-f4' -SettleMs 800 | Out-Null
    $sess = Get-SessionText $d
    $err = Read-Stderr $app
    "tabs now: $(([regex]::Matches($sess, 'path = ''([^'']+)''') | ForEach-Object { Split-Path -Leaf $_.Groups[1].Value }) -join ',') (F4 should have opened b2.txt/c3.txt at the hit: inspect f1/f2)"
} }

# ======================================================================= g
if (Want 'g') { Invoke-Scenario 'g' 'user tool (echo) with streamed output' {
    $d = New-TestEnv $DataRoot 'g'
    Write-Utf8 "$d\files\g.txt" "tool`n"
    $toml = @'
[[tool]]
name = "Stream"
command = "cmd"
args = ["/c", "for /L %i in (1,1,6) do (echo streamed-line-%i & ping -n 2 127.0.0.1 >nul)"]
group = 1
slot = 1
output = "panel"
'@
    Write-Utf8 "$d\settings\tools.toml" $toml
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\g.txt") -Tag 'g'
    Send-Chord $app 'ctrl+1'
    Save-Shot $app 'g1-tool-running' -SettleMs 2000 | Out-Null
    Save-Shot $app 'g2-tool-finished' -SettleMs 5000 | Out-Null
    $err = Read-Stderr $app
    'Ctrl+1 pressed (AZERTY layout: digit needs Shift); inspect g1 (partial output, streaming) and g2 (all 6 lines)'
} }

# ======================================================================= h
if (Want 'h') { Invoke-Scenario 'h' 'UTF-16 and CRLF files: status bar + byte round trip' {
    $d = New-TestEnv $DataRoot 'h'
    $u16 = [Text.Encoding]::Unicode
    [IO.File]::WriteAllBytes("$d\files\u16.txt", ([byte[]](0xFF, 0xFE)) + $u16.GetBytes("h$([char]0xE9)llo utf16`r`nline two`r`n"))
    [IO.File]::WriteAllBytes("$d\files\crlf.txt", [Text.Encoding]::ASCII.GetBytes("one`r`ntwo`r`nthree`r`n"))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\u16.txt", "$d\files\crlf.txt") -Tag 'h'
    Save-Shot $app 'h1-crlf-active' | Out-Null     # last opened tab is active
    Send-Text $app 'Z'; Send-Chord $app 'ctrl+s'
    Send-Chord $app 'ctrl+pgup'
    Save-Shot $app 'h2-utf16-active' | Out-Null
    Send-Text $app 'Y'; Send-Chord $app 'ctrl+s'
    Start-Sleep -Milliseconds 600
    $cb = [IO.File]::ReadAllBytes("$d\files\crlf.txt"); $ub = [IO.File]::ReadAllBytes("$d\files\u16.txt")
    $crlfOk = ([Text.Encoding]::ASCII.GetString($cb) -eq "Zone`r`ntwo`r`nthree`r`n")
    $u16Ok = ($ub[0] -eq 0xFF -and $ub[1] -eq 0xFE -and $u16.GetString($ub, 2, $ub.Length - 2) -eq "Yh$([char]0xE9)llo utf16`r`nline two`r`n")
    Assert $crlfOk "CRLF file changed: $([BitConverter]::ToString($cb))"
    Assert $u16Ok "UTF-16 file changed: $([BitConverter]::ToString($ub))"
    'CRLF and UTF-16LE+BOM files keep EOL/BOM/encoding byte for byte after edit+save; status-bar panes: see h1/h2'
} }

# ======================================================================= i
if (Want 'i' -and -not $SkipBig) { Invoke-Scenario 'i' '100 MB file: open time and typing latency' {
    $d = New-TestEnv $DataRoot 'i'
    $big = "$d\files\big.txt"
    $line = ('0123456789abcdefghij' * 5).Substring(0, 99) + "`n"
    $chunk = $line * 1000
    $fs = [IO.File]::Create($big)
    $bytes = [Text.Encoding]::ASCII.GetBytes($chunk)
    for ($i = 0; $i -lt 1050; $i++) { $fs.Write($bytes, 0, $bytes.Length) }
    $fs.Close()
    $mb = [math]::Round((Get-Item $big).Length / 1MB, 1)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big) -Tag 'i'
    $cold = Get-ColdStartMs $app
    Save-Shot $app 'i1-big-opened' | Out-Null
    $lat = @()
    foreach ($k in 'x', 'y', 'z') {
        $h0 = Get-RegionHash $app
        $sw = [Diagnostics.Stopwatch]::StartNew()
        Send-Vk ([W32]::VkKeyScan([char]$k) -band 0xFF) $false; Send-Vk ([W32]::VkKeyScan([char]$k) -band 0xFF) $true
        $changed = $false
        while ($sw.ElapsedMilliseconds -lt 8000) { if ((Get-RegionHash $app) -ne $h0) { $changed = $true; break } }
        $lat += $(if ($changed) { $sw.ElapsedMilliseconds } else { -1 })
        Assert-Focus $app
    }
    $h0 = Get-RegionHash $app; $sw = [Diagnostics.Stopwatch]::StartNew()
    Send-Chord $app 'ctrl+end' -PauseMs 0
    $changed = $false
    while ($sw.ElapsedMilliseconds -lt 8000) { if ((Get-RegionHash $app) -ne $h0) { $changed = $true; break } }
    $endMs = $(if ($changed) { $sw.ElapsedMilliseconds } else { -1 })
    Save-Shot $app 'i2-big-after-typing' | Out-Null
    $m = Get-Mem $app
    $err = Read-Stderr $app
    $lines = [regex]::Match($err, 'loaded (\d+) lines').Groups[1].Value
    $detail = "file=${mb}MB lines=$lines windowMs(after spawn)=$($app.WindowMs) coldStartMs(app-reported)=$cold typing latency ms(x,y,z; status-bar repaint incl. ~40ms capture cost)=$($lat -join '/') ctrl+end ms=$endMs ws=$($m.WorkingSetMB)MB peak=$($m.PeakMB)MB"
    Remove-Item $big -Force -ErrorAction SilentlyContinue
    Assert (@($lat | Where-Object { $_ -lt 0 }).Count -eq 0) "typing never repainted: $detail"
    $detail
} }

# ======================================================================= j
if (Want 'j') { Invoke-Scenario 'j' 'single instance: 2nd launch forwards the file' {
    $d = New-TestEnv $DataRoot 'j'
    Write-Utf8 "$d\files\first.txt" "first`n"
    Write-Utf8 "$d\files\second.txt" "second`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\first.txt") -Extra @() -Tag 'j1'
    $e1 = Read-Stderr $app
    $w2 = $null
    $p2 = Start-App -Exe $Exe -AppArgs @('--settings-dir', "`"$d\settings`"", "`"$d\files\second.txt`"") -WorkDir "$d\files" -Tag 'j2'
    $exited = $p2.Proc.WaitForExit(15000)
    $err2 = Read-Stderr $p2
    $app.Proc.Refresh()
    Start-Sleep -Milliseconds 800
    $focusNow = (Get-ForegroundPid) -eq $app.Proc.Id
    if ($focusNow) { Save-Shot $app 'j1-after-forward' | Out-Null } else { Set-AppFocus $app; Save-Shot $app 'j1-after-forward' | Out-Null }
    $sess = Get-SessionText $d
    $has = $sess -like '*second.txt*'
    $detail = "2nd process exited=$exited ($($p2.Proc.ExitCode)); 2nd stderr='$($err2.Trim())'; second.txt in 1st instance session=$has"
    Assert ($err2 -match 'forwarded') "not forwarded: $detail ; 1st stderr: $e1"
    Assert $has "file not opened in first instance: $detail"
    $detail
} }

# ======================================================================= k
if (Want 'k') { Invoke-Scenario 'k' 'hard kill then relaunch: recovery' {
    $d = New-TestEnv $DataRoot 'k'
    Write-Utf8 "$d\files\k.txt" "original`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\k.txt") -Tag 'k1'
    Send-Text $app 'UNSAVED_K'
    Save-Shot $app 'k1-typed-unsaved' | Out-Null
    $jdir = "$d\settings"
    # the app autosaves to its journal every 30 s: wait for a journal file (max 45 s)
    $got = Wait-Until { @(Get-ChildItem $jdir -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.FullName -match 'journal|recover' }).Count -gt 0 } 45000
    $files = (Get-ChildItem $jdir -Recurse -File | ForEach-Object { $_.FullName.Substring($jdir.Length + 1) }) -join ', '
    Kill-App $app
    Start-Sleep -Milliseconds 500
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Tag 'k2'
    Save-Shot $app2 'k2-relaunch' | Out-Null
    $err2 = Read-Stderr $app2
    $rec = [regex]::Match($err2, '(\d+) document\(s\) recovered').Groups[1].Value
    $disk = (Get-Content -Raw "$d\files\k.txt")
    $detail = "journal appeared=$got files=[$files]; recoveredDocs=$rec; disk still '$($disk.Trim())'; look at k2-relaunch.png for the recovered text/dirty state (no modal prompt is expected: the app reopens recovered docs dirty)"
    Assert ($rec -ne '' -and [int]$rec -ge 1) "nothing recovered: $detail"
    $detail
} }

# ======================================================================= l
if (Want 'l') {
    $samples = [ordered]@{
        'c'    = "#include <stdio.h>`n/* block comment */`nint main(void) {`n    // line comment`n    const char *s = `"hello`";`n    int n = 0x2A + 42;`n    for (int i = 0; i < n; i++) { printf(`"%d\n`", i); }`n    return 0;`n}`n"
        'php'  = "<html>`n<body>`n<?php`n// php comment`n`$x = 42;`nif (`$x > 1) { echo `"hello`"; }`nfunction foo(`$a) { return `$a + 1; }`n?>`n<p class=`"a`">text</p>`n</body>`n</html>`n"
        'html' = "<!DOCTYPE html>`n<html lang=`"en`">`n<!-- comment -->`n<head><title>T</title><style>p { color: #f00; }</style></head>`n<body><a href=`"x.html`">link</a><script>var n = 42; // js`n</script></body>`n</html>`n"
        'sql'  = "-- comment`nSELECT id, name FROM users WHERE age > 42 AND name = 'bob'`nORDER BY id DESC;`n/* block */`nINSERT INTO t (a, b) VALUES (1, 'x');`n"
        'py'   = "#!/usr/bin/env python3`n# comment`nimport os`ndef foo(x, y=42):`n    `"`"`"doc`"`"`"`n    s = 'hello'`n    if x > 1 and y:`n        return s + str(x)`nclass A: pass`n"
        'rs'   = "// comment`nuse std::fmt;`nfn main() {`n    let s = `"hello`";`n    let n: u32 = 42;`n    if n > 1 { println!(`"{}`", s); }`n}`nstruct A { x: i32 }`n"
        'json' = "{`n  `"name`": `"brouillon`",`n  `"n`": 42,`n  `"ok`": true,`n  `"list`": [1, 2, null]`n}`n"
        'md'   = "# Title`n`nSome **bold** and *italic* text with ``code``.`n`n- item one`n- item two`n`n> quote`n`n[link](http://x.y)`n"
        'sh'   = "#!/bin/bash`n# comment`nNAME=`"world`"`nif [ -z `"`$NAME`" ]; then`n  echo 'hello'`nfi`nfor i in 1 2 3; do echo `$i; done`n"
    }
    foreach ($ext in $samples.Keys) {
        Invoke-Scenario "l-$ext" "syntax colouring of a .$ext sample" {
            $d = New-TestEnv $DataRoot "l$ext"
            $f = "$d\files\sample.$ext"
            Write-Utf8 $f $samples[$ext]
            $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag "l-$ext"
            $png = Save-Shot $app "l-$ext" -SettleMs 800
            $err = Read-Stderr $app
            $hl = [regex]::Match($err, '\[bg-parse\] done[^\r\n]*').Value
            "capture $png ; $hl"
        }
    }
}

# ----------------------------------------------------------------- summary
$out = Get-SmokeOut
$script:Results | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $out 'results.json')
Write-Host ''
$script:Results | Format-Table Id, Status, Seconds, Title -AutoSize | Out-String | Write-Host
if (-not $KeepData) { Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue }
