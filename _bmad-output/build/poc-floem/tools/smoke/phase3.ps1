<#
.SYNOPSIS
  Phase 3 scenarios: folder tree, guides/ruler/minimap, distraction-free, diff, Emmet, macros, last-visited tab, bookmarks list.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\phase3.ps1 -Exe C:\path\brouillon.exe [-Only tree,emmet]
  Same safety contract as run.ps1 (keys only while our window is foreground; TEMPORARY --settings-dir; process killed).
  Takes the cross-agent desktop lock (gui-lock.ps1) for the whole run.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-p3'), [string]$Tag = '', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot $(if ($env:BROUILLON_SMOKE_TI -eq '1') { 'testinput.ps1' } else { 'lib.ps1' }))
. (Join-Path $PSScriptRoot 'gui-lock.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })   # `-File ... -Only a,b` arrives as one string
function Want($n) { $Only.Count -eq 0 -or $Only -contains $n }

Enter-GuiLock
try {

if ((Want 'tree') -and $env:BROUILLON_STALE_TREE -eq '1') {  # RETIRED: stale pixel rows and removed per-workspace state; use phasepass.ps1 -Only tree
Invoke-Scenario 'tree' 'Folder tree: open folder, lazy expand, keyboard, double-click opens, remembered per workspace' {
    $d = New-TestEnv $DataRoot 't'
    $proj = "$d\proj"
    New-Item -ItemType Directory -Force "$proj\src\inner", "$proj\docs" | Out-Null
    [IO.File]::WriteAllText("$proj\README.md", "# readme`n", $u)
    [IO.File]::WriteAllText("$proj\src\main.rs", "fn main() {}`n", $u)
    [IO.File]::WriteAllText("$proj\src\inner\deep.txt", "deep`n", $u)
    [IO.File]::WriteAllText("$proj\docs\guide.md", "guide`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Extra @('--new-window', '--no-session', '--folder', $proj) -Tag 'tree'
    Save-Shot $app "${Tag}tree-0-opened" | Out-Null
    # rows (20 px) start under the header: docs, src, README.md
    $y0 = [double]$env:P3_TREE_Y0; if ($y0 -eq 0) { $y0 = 154 }   # was 178 before f694fd3 removed the 24 px toolbar row (-24); not re-measured
    Send-Click $app 60 ($y0 + 20)                     # click `src` -> expands
    Save-Shot $app "${Tag}tree-1-src-expanded" | Out-Null
    Send-Chord $app 'down' 200                        # inner
    Send-Chord $app 'right' 200                       # expand inner
    Send-Chord $app 'down' 200                        # deep.txt
    Save-Shot $app "${Tag}tree-2-keyboard" | Out-Null
    Send-Chord $app 'enter' 700                       # opens deep.txt
    Save-Shot $app "${Tag}tree-3-enter-opened" | Out-Null
    # double-click README.md (row index after expansion: docs, src, inner, deep.txt, main.rs, README.md)
    Send-Click $app 60 ($y0 + 20 * 5) -Double
    Save-Shot $app "${Tag}tree-4-dblclick-opened" | Out-Null
    $err = Read-Stderr $app
    Stop-AppGraceful $app | Out-Null
    $st = Get-ChildItem "$d\settings\workspaces" -Filter *.toml | Select-Object -First 1
    if (-not $st) { throw 'no per-workspace state file written' }
    $txt = Get-Content -Raw $st.FullName
    if ($txt -notmatch 'inner') { throw "expanded state not saved: $txt" }
    # second launch: expansion remembered, tree reopens by itself (last workspace)
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Extra @('--new-window', '--no-session') -Tag 'tree2'
    Save-Shot $app2 "${Tag}tree-5-restored" | Out-Null
    'ok'
}
}

if (Want 'view') {
Invoke-Scenario 'view' 'Indent guides, column ruler (settings), toggles persisted, distraction-free mode' {
    $d = New-TestEnv $DataRoot 'v'
    [IO.File]::WriteAllText("$d\settings\settings.toml", "[editor]`nruler_column = 40`n", $u)
    $f = "$d\files\sample.py"
    $src = "import os`n`ndef alpha():`n    if True:`n        return 1`n`n    return 2`n`nclass Beta:`n    def gamma(self):`n        for i in range(3):`n            print(i)`n"
    [IO.File]::WriteAllText($f, $src, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'view'
    Save-Shot $app "${Tag}view-1-guides-ruler" | Out-Null
    # toggle guides off through the palette
    Send-Chord $app 'ctrl+k' 500
    Send-Text $app 'indent guides'
    Send-Chord $app 'enter' 500
    Save-Shot $app "${Tag}view-2-guides-off" | Out-Null
    $st = Get-Content -Raw "$d\settings\settings.toml"
    if ($st -notmatch 'indent_guides\s*=\s*false') { throw "indent_guides not saved: $st" }
    Send-Chord $app 'ctrl+k' 500
    Send-Text $app 'column ruler'
    Send-Chord $app 'enter' 500
    Save-Shot $app "${Tag}view-3-ruler-off" | Out-Null
    $st = Get-Content -Raw "$d\settings\settings.toml"
    if ($st -notmatch 'ruler_column\s*=\s*0') { throw "ruler_column not saved: $st" }
    Send-Chord $app 'ctrl+alt+z' 700
    Save-Shot $app "${Tag}view-4-distraction-free" | Out-Null
    Send-Chord $app 'ctrl+alt+z' 700
    Save-Shot $app "${Tag}view-5-back" | Out-Null
    'ok'
}
}

if (Want 'minimap') {
Invoke-Scenario 'minimap' 'Minimap: overview, viewport marker, click moves the caret/scroll' {
    $d = New-TestEnv $DataRoot 'm'
    [IO.File]::WriteAllText("$d\settings\settings.toml", "[editor]`nminimap = true`n", $u)
    $f = "$d\files\big.py"
    $sb = New-Object Text.StringBuilder
    for ($i = 1; $i -le 3000; $i++) { [void]$sb.Append("def f$i(x):`n    if x:`n        return $i`n`n") }
    [IO.File]::WriteAllText($f, $sb.ToString(), $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'map'
    Save-Shot $app "${Tag}map-1-top" | Out-Null
    Send-Click $app 1040 420        # middle of the minimap
    Save-Shot $app "${Tag}map-2-clicked-middle" | Out-Null
    Send-Click $app 1040 600
    Save-Shot $app "${Tag}map-3-clicked-lower" | Out-Null
    # a click in the TEXT (editor still receives mouse events) then typing edits where the minimap sent us
    Send-Click $app 400 200
    Send-Text $app 'Z'
    Save-Shot $app "${Tag}map-4-typed" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t.Length -ne $sb.Length + 1) { throw 'typing after a click in the text did not edit the document' }
    if ($t.StartsWith('Z') -or $t.StartsWith('def f1(x):') -eq $false) { throw 'edit landed at the top of the file: the minimap click did not scroll' }
    'ok'
}
}

if (Want 'diff') {
Invoke-Scenario 'diff' 'Compare: buffer vs disk, side by side / inline, last visited tab' {
    $d = New-TestEnv $DataRoot 'd'
    $a = "$d\files\a.txt"; $b = "$d\files\b.txt"
    [IO.File]::WriteAllText($a, "one`ntwo`nthree`nfour`nfive`n", $u)
    [IO.File]::WriteAllText($b, "one`n2`nthree`nfour`nfive`nsix`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($a, $b) -Tag 'diff'
    Send-Chord $app 'ctrl+tab' 500                       # now on the other tab: the previous one is the "last visited"
    Send-Chord $app 'ctrl+k' 500
    Send-Text $app 'compare with last'
    Send-Chord $app 'enter' 700
    Save-Shot $app "${Tag}diff-1-last-tab-side" | Out-Null
    Send-Chord $app 'esc' 500
    # edit the active buffer, then compare with the disk
    Send-Chord $app 'ctrl+home' 200
    Send-Text $app 'EDITED '
    Send-Chord $app 'ctrl+k' 500
    Send-Text $app 'compare with file on disk'
    Send-Chord $app 'enter' 700
    Save-Shot $app "${Tag}diff-2-disk" | Out-Null
    Send-Chord $app 'esc' 500
    Save-Shot $app "${Tag}diff-3-closed" | Out-Null
    # nothing was written by comparing
    if ([IO.File]::ReadAllText($a) -cne "one`ntwo`nthree`nfour`nfive`n") { throw 'a.txt changed on disk' }
    if ([IO.File]::ReadAllText($b) -cne "one`n2`nthree`nfour`nfive`nsix`n") { throw 'b.txt changed on disk' }
    'ok'
}
}

if (Want 'emmet') {
Invoke-Scenario 'emmet' 'Emmet: Tab expands in .html and .css, Ctrl+E too, other files untouched' {
    $d = New-TestEnv $DataRoot 'e'
    $h = "$d\files\t.html"; $c = "$d\files\s.css"; $n = "$d\files\n.txt"
    foreach ($p in @($h, $c, $n)) { [IO.File]::WriteAllText($p, '', $u) }
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($h) -Tag 'emmet'
    Send-Text $app 'ul>li.item$*3'
    Send-Chord $app 'tab' 500
    Save-Shot $app "${Tag}emmet-1-html" | Out-Null
    Send-Text $app 'X'
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($h)
    if ($t -notmatch '<ul>' -or $t -notmatch 'class="item3"') { throw "html not expanded: $t" }
    if ($t -notmatch '<li class="item1">X</li>') { throw "caret did not land inside the first <li>: $t" }
    # Ctrl+E (command) on a fresh line
    1..3 | ForEach-Object { Send-Chord $app 'down' 100 }
    Send-Chord $app 'end' 200
    Send-Chord $app 'enter' 200
    Send-Text $app 'a.go'                           # (braces need AltGr on AZERTY: not typed reliably by the harness)
    Send-Chord $app 'ctrl+e' 500
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($h)
    if ($t -notmatch '<a href="" class="go"></a>') { throw "ctrl+e failed: [$($t -replace "`n",'|')]" }
    Save-Shot $app "${Tag}emmet-2-ctrl-e" | Out-Null
    'ok'
}
Invoke-Scenario 'emmet2' 'Emmet in CSS, and Tab in a .txt file' {
    $d = New-TestEnv $DataRoot 'e2'
    $c = "$d\files\s.css"; $n = "$d\files\n.txt"
    foreach ($p in @($c, $n)) { [IO.File]::WriteAllText($p, '', $u) }
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($c, $n) -Tag 'emmet2'
    Send-Chord $app 'ctrl+shift+tab' 400            # to s.css
    Send-Text $app 'm10-20'
    Send-Chord $app 'tab' 400
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($c)
    if ($t -ne 'margin: 10px 20px;') { throw "css not expanded: [$t]" }
    Send-Chord $app 'ctrl+tab' 400                  # to n.txt
    Send-Text $app 'abc'
    Send-Chord $app 'tab' 300
    Send-Text $app 'd'
    Save-Shot $app "${Tag}emmet2-txt-tab" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $x = [IO.File]::ReadAllText($n)
    "txt after Tab: [" + ($x -replace "`t", '<TAB>') + "]"
}
}

if (Want 'macro') {
Invoke-Scenario 'macro' 'Macro: record (Ctrl+Q in Classic), play twice, saved to macro.toml' {
    $d = New-TestEnv $DataRoot 'mac'
    [IO.File]::WriteAllText("$d\settings\settings.toml", "[keymap]`nclassic = true`n", $u)
    $f = "$d\files\m.txt"
    [IO.File]::WriteAllText($f, "a`nb`nc`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($f) -Tag 'macro'
    Send-Chord $app 'ctrl+q' 500                    # start recording (must NOT quit the program)
    if ($app.Proc.HasExited) { throw 'Ctrl+Q quit the program instead of recording' }
    Send-Text $app 'X'
    Send-Chord $app 'down' 150
    Send-Chord $app 'home' 150
    Send-Chord $app 'ctrl+q' 500                    # stop
    Save-Shot $app "${Tag}macro-1-recorded" | Out-Null
    Send-Chord $app 'ctrl+shift+q' 500              # play
    Send-Chord $app 'ctrl+shift+q' 500              # play again
    Save-Shot $app "${Tag}macro-2-played" | Out-Null
    Send-Chord $app 'ctrl+s' 500
    $t = [IO.File]::ReadAllText($f)
    if ($t -cne "Xa`nXb`nXc`n") { throw "macro result wrong: $($t -replace "`n",'|')" }
    if (-not (Test-Path "$d\settings\macro.toml")) { throw 'macro.toml not written' }
    'ok: ' + ((Get-Content -Raw "$d\settings\macro.toml") -replace "`r?`n", ' ')
}
}

if (Want 'lists') {
Invoke-Scenario 'lists' 'Last visited tab, bookmarks list panel' {
    $d = New-TestEnv $DataRoot 'li'
    $a = "$d\files\a.txt"; $b = "$d\files\b.txt"
    [IO.File]::WriteAllText($a, "a1`na2`na3`na4`n", $u)
    [IO.File]::WriteAllText($b, "b1`nb2`nb3`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($a, $b) -Tag 'lists'
    Send-Chord $app 'ctrl+tab' 500                  # other tab
    Save-Shot $app "${Tag}lists-1-on-other" | Out-Null
    Send-Chord $app 'ctrl+k' 500; Send-Text $app 'last visited'; Send-Chord $app 'enter' 700
    Save-Shot $app "${Tag}lists-2-last-visited" | Out-Null
    # bookmarks on lines 2 and 4 of the active tab, then the list
    Send-Chord $app 'down' 150
    Send-Chord $app 'ctrl+k' 500; Send-Text $app 'bookmarks: toggle'; Send-Chord $app 'enter' 500
    Send-Chord $app 'down' 150; Send-Chord $app 'down' 150
    Send-Chord $app 'ctrl+k' 500; Send-Text $app 'bookmarks: toggle'; Send-Chord $app 'enter' 500
    Send-Chord $app 'ctrl+k' 500; Send-Text $app 'bookmarks: list'; Send-Chord $app 'enter' 800
    Save-Shot $app "${Tag}lists-3-bookmarks" | Out-Null
    'ok'
}
}

if (Want 'perfmap') {
# 100 MB typing latency (status-bar repaint after one key), minimap off then on.
$root = 'D:\travail\_perfE'; New-Item -ItemType Directory -Force $root | Out-Null
$big = Join-Path $root 'big100.txt'
if (-not (Test-Path $big)) {
    $line = ('abcdefghij klmnopqrst uvwxyz 0123456789 ' * 3).Substring(0, 98) + "`n"
    $bytes = [Text.Encoding]::ASCII.GetBytes($line)
    $chunk = New-Object byte[] ($bytes.Length * 10000)
    for ($i = 0; $i -lt 10000; $i++) { [Array]::Copy($bytes, 0, $chunk, $i * $bytes.Length, $bytes.Length) }
    $fs = [IO.File]::Create($big); $w = 0L
    while ($w -lt 100MB) { $fs.Write($chunk, 0, $chunk.Length); $w += $chunk.Length }
    $fs.Close()
}
foreach ($mm in @('false', 'true')) {
    Invoke-Scenario "perfmap-$mm" "100 MB typing latency, minimap=$mm" {
        $d = New-TestEnv $DataRoot "pm$mm"
        [IO.File]::WriteAllText("$d\settings\settings.toml", "[editor]`nminimap = $mm`n", $u)
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @($big) -Tag "perfmap-$mm"
        Start-Sleep -Milliseconds 3000; Set-AppFocus $app
        $r = New-Object RECT; [void][W32]::GetWindowRect($app.Hwnd, [ref]$r)
        $sc = [W32]::GetDpiForWindow($app.Hwnd) / 96.0
        $strip = @(0.0, (($r.Bottom - $r.Top) / $sc - 30.0), (($r.Right - $r.Left) / $sc), 24.0)
        $lat = @()
        for ($i = 0; $i -lt 14; $i++) {
            $script:Deadline = (Get-Date).AddSeconds(40)
            Assert-Focus $app
            $before = Get-RegionHash -App $app -Logical $strip
            $sw = [Diagnostics.Stopwatch]::StartNew()
            [void][W32]::Key([uint16][int][char]'X', $false, $false, $false, 0)
            [void][W32]::Key([uint16][int][char]'X', $true, $false, $false, 0)
            $ms = -1
            while ($sw.ElapsedMilliseconds -lt 4000) { if ((Get-RegionHash -App $app -Logical $strip) -ne $before) { $ms = $sw.ElapsedMilliseconds; break } }
            if ($ms -ge 0 -and $i -ge 2) { $lat += $ms }
            Start-Sleep -Milliseconds 250
        }
        Save-Shot $app "${Tag}perfmap-$mm" | Out-Null
        $v = @($lat | Sort-Object)
        "n=$($v.Count) min=$($v[0]) median=$($v[[int][math]::Floor(($v.Count-1)/2)]) max=$($v[-1]) ms"
    }
}
}

$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize | Out-String | Write-Host

} finally { Exit-GuiLock }
