<#
.SYNOPSIS
  Phase 2/3/4 verification pass on a DEV exe (--features test-input). Everything goes through the test-input pipe
  (no SendInput), TEMPORARY --settings-dir. Pixel coordinates are SCREENSHOT pixels at 1.75 scale (client area).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\phasepass.ps1 -Exe <exe> [-Only tree,qo] [-Tag x-]
  Scenarios: shortcuts, encoding, qo (Ctrl+P @symbol, :line), tree, nav, mcp-default (the link is OFF out of the box).
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-phasepass'), [string]$Tag = '', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
$env:BROUILLON_REMOTE_DEBUG = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
function Want($n) { $Only.Count -eq 0 -or $Only -contains $n }
function Shot($app, $name) { Save-Shot $app "$Tag$name" 400 | Out-Null }
function Px($app, [double]$x, [double]$y, [switch]$Double) {
    $n = 1; if ($Double) { $n = 2 }
    [void](Invoke-TI $app @{ op = 'click'; x = ($x / 1.75); y = ($y / 1.75); count = $n })
    Start-Sleep -Milliseconds 350
}
function Palette($app, [string]$cmd) {
    Send-Chord $app 'ctrl+k' 250
    $null = Wait-State $app { param($s) $s.state.palette_open }
    Send-Text $app $cmd
    Start-Sleep -Milliseconds 250
    Send-Chord $app 'enter' 700
}
function Dbg($app, [string]$pat) { @((Get-State $app 200).lines | Where-Object { $_ -match $pat }) }

if (Want 'shortcuts') { Invoke-Scenario 'shortcuts' 'Keyboard Shortcuts editor: open, search, record, save to keymap.toml' {
    $d = New-TestEnv $DataRoot 'sc'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`nbeta`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'sc'
    Send-Chord $app 'ctrl+k' 250
    Send-Text $app 'Keyboard Shortcuts'
    Shot $app 'pp-sc-0-palette'
    Send-Chord $app 'enter' 900
    Shot $app 'pp-sc-1-open'
    Send-Text $app 'duplicate line'
    Start-Sleep -Milliseconds 400
    Shot $app 'pp-sc-2-search'
    'opened'
} }

if (Want 'encoding') { Invoke-Scenario 'encoding' 'Encoding picker (palette) and status-bar encoding cell' {
    $d = New-TestEnv $DataRoot 'enc'
    [IO.File]::WriteAllBytes("$d\files\latin.txt", [Text.Encoding]::GetEncoding(1252).GetBytes("caf" + [char]0xE9 + " na" + [char]0xEF + "ve`n"))
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\latin.txt") -Tag 'enc'
    Shot $app 'pp-enc-0-open'
    Send-Chord $app 'ctrl+k' 250
    Send-Text $app 'Encoding'
    Start-Sleep -Milliseconds 300
    Shot $app 'pp-enc-1-palette'
    Send-Chord $app 'esc' 300
    # status-bar encoding cell (screenshot px 420..580, y 1243)
    Px $app 500 1243
    Shot $app 'pp-enc-2-cell-click'
    'shots'
} }

if (Want 'qo') { Invoke-Scenario 'qo' 'Quick open: @symbol and :line' {
    $d = New-TestEnv $DataRoot 'qo'
    [IO.File]::WriteAllText("$d\files\code.rs", "fn alpha() {}`n`nfn beta_symbol() {}`n`nstruct Gamma;`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\code.rs") -Tag 'qo'
    Send-Chord $app 'ctrl+p' 300
    Send-Text $app '@beta'
    Start-Sleep -Milliseconds 400
    Shot $app 'pp-qo-0-at-beta'
    Send-Chord $app 'enter' 500
    $s = Get-State $app
    Shot $app 'pp-qo-1-after-enter'
    if (($s.state.caret -join ' ') -ne '3 1') { throw "caret after @beta: $($s.state.caret)" }
    "caret=$($s.state.caret)"
} }

if (Want 'tree') { Invoke-Scenario 'tree' 'Folder tree / workspace: --folder, expand, open, per-workspace state' {
    $d = New-TestEnv $DataRoot 'tr'
    $proj = "$d\proj"
    New-Item -ItemType Directory -Force "$proj\src\inner", "$proj\docs" | Out-Null
    [IO.File]::WriteAllText("$proj\README.md", "# readme`n", $u)
    [IO.File]::WriteAllText("$proj\src\main.rs", "fn main() {}`n", $u)
    [IO.File]::WriteAllText("$proj\src\inner\deep.txt", "deep`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Extra @('--new-window', '--no-session', '--folder', $proj) -Tag 'tr'
    Shot $app 'pp-tree-0'
    Px $app 60 733            # src
    Shot $app 'pp-tree-1-src'
    Px $app 60 768            # first child (inner) -> expand
    Px $app 90 803 -Double    # main.rs or deep.txt
    Start-Sleep -Milliseconds 500
    Shot $app 'pp-tree-2-opened'
    $s = Get-State $app
    $titles = ($s.state.tab_titles -join ',')
    Stop-AppGraceful $app | Out-Null
    $wd = "$d\settings\workspaces"
    $files = if (Test-Path $wd) { @(Get-ChildItem $wd).Count } else { 0 }
    "tabs=$titles workspace-state-files=$files"
} }

if (Want 'nav') { Invoke-Scenario 'nav' 'Navigation history: Alt+Left after two jumps' {
    $d = New-TestEnv $DataRoot 'nav'
    $t = (1..60 | ForEach-Object { "line $_" }) -join "`n"
    [IO.File]::WriteAllText("$d\files\n.txt", $t, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\n.txt") -Tag 'nav'
    Send-Chord $app 'ctrl+g' 300; Send-Text $app '40'; Send-Chord $app 'enter' 400
    $a = (Get-State $app).state.caret
    Send-Chord $app 'ctrl+g' 300; Send-Text $app '10'; Send-Chord $app 'enter' 400
    $b = (Get-State $app).state.caret
    Send-Chord $app 'alt+left' 400
    $c = ((Get-State $app).state.caret -join ' ')
    Shot $app 'pp-nav-0'
    Send-Chord $app 'alt+left' 400; $d2 = ((Get-State $app).state.caret -join ' ')
    Send-Chord $app 'alt+right' 400; $e = ((Get-State $app).state.caret -join ' ')
    Send-Chord $app 'alt+right' 400; $f = ((Get-State $app).state.caret -join ' ')
    if ($c -ne '40 1' -or $d2 -ne '1 1' -or $e -ne '40 1' -or $f -ne '10 1') { throw "nav: back=[$c] back2=[$d2] fwd=[$e] fwd2=[$f] (want 40 1 / 1 1 / 40 1 / 10 1)" }
    "goto40 then goto10, Alt+Left x2 -> [$c] [$d2], Alt+Right x2 -> [$e] [$f]"
} }

if (Want 'nav-tabs') { Invoke-Scenario 'nav-tabs' 'Navigation history across a tab switch and a Ctrl+P @symbol jump' {
    $d = New-TestEnv $DataRoot 'navt'
    [IO.File]::WriteAllText("$d\files\b.txt", ((1..60 | ForEach-Object { "a line $_" }) -join "`n"), $u)
    [IO.File]::WriteAllText("$d\files\a.rs", "fn one() {}`n`nfn two() {}`n`nfn three() {}`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.rs", "$d\files\b.txt") -Tag 'navt'
    $s0 = Get-State $app; $first = $s0.state.tab_title
    Send-Chord $app 'ctrl+pgdn' 400; $other = (Get-State $app).state.tab_title
    Send-Chord $app 'ctrl+pgdn' 400
    if ((Get-State $app).state.tab_title -ne $first) { throw 'tab cycling did not return' }
    Send-Chord $app 'ctrl+g' 300; Send-Text $app '30'; Send-Chord $app 'enter' 400
    Send-Chord $app 'ctrl+pgdn' 400
    $t1 = (Get-State $app).state.tab_title
    Send-Chord $app 'ctrl+p' 300; Send-Text $app '@three'; Start-Sleep -Milliseconds 300; Send-Chord $app 'enter' 500
    $c1 = ((Get-State $app).state.caret -join ' ')
    Send-Chord $app 'alt+left' 500; $c2 = ((Get-State $app).state.caret -join ' ')
    Send-Chord $app 'alt+left' 500; $s = (Get-State $app).state; $t3 = $s.tab_title; $c3 = ($s.caret -join ' ')
    Shot $app 'pp-navt-0-back-to-first-tab'
    Send-Chord $app 'alt+right' 500; $s = (Get-State $app).state; $t4 = $s.tab_title
    if ($t1 -ne $other -or $c1 -ne '5 1' -or $c2 -ne '1 1' -or $t3 -ne $first -or $c3 -ne '30 1' -or $t4 -ne $other) { throw "nav-tabs: t1=$t1 c1=[$c1] after-back=[$c2] then tab=$t3 caret=[$c3] fwd tab=$t4" }
    "symbol jump to [$c1], Alt+Left -> [$c2], Alt+Left -> tab '$t3' [$c3], Alt+Right -> tab '$t4'"
} }

if (Want 'switcher') { Invoke-Scenario 'switcher' 'Ctrl+Tab popup: MRU list, Ctrl release commits, Esc cancels' {
    $d = New-TestEnv $DataRoot 'sw'
    foreach ($n in 'a', 'b', 'c') { [IO.File]::WriteAllText("$d\files\$n.txt", "file $n`n", $u) }
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt", "$d\files\b.txt", "$d\files\c.txt") -Tag 'sw'
    $start = (Get-State $app).state.tab_title
    # visit history: start (c), then b
    Send-Chord $app 'ctrl+pgup' 400
    $mid = (Get-State $app).state.tab_title
    Send-Hold $app 'ctrl'
    Send-Tap $app 'ctrl+tab' 500
    Shot $app 'pp-sw-0-open'
    $still = (Get-State $app).state.tab_title
    Send-Tap $app 'ctrl+tab' 500
    Shot $app 'pp-sw-1-second'
    Send-Release $app 'ctrl'
    $end = (Get-State $app).state.tab_title
    if ($still -ne $mid) { throw "the tab changed while the popup was open ($mid -> $still)" }
    if ($end -eq $mid -or $end -eq $start) { throw "commit chose '$end' (start '$start', before popup '$mid'): want the third, least recent tab" }
    # Esc cancels
    Send-Hold $app 'ctrl'
    Send-Tap $app 'ctrl+tab' 400
    Send-Tap $app 'ctrl+esc' 400
    Send-Release $app 'ctrl'
    $after = (Get-State $app).state.tab_title
    if ($after -ne $end) { throw "Esc did not cancel: $end -> $after" }
    # a single Ctrl+Tab press-and-release goes to the previously used tab
    Send-Hold $app 'ctrl'
    Send-Tap $app 'ctrl+tab' 300
    Send-Release $app 'ctrl'
    $prev = (Get-State $app).state.tab_title
    if ($prev -ne $mid) { throw "quick Ctrl+Tab went to '$prev', want the previous tab '$mid'" }
    "start=$start mid=$mid popup-1st/2nd committed=$end, Esc cancelled, quick toggle -> $prev"
} }

if (Want 'reopen') { Invoke-Scenario 'reopen' 'Encoding picker: decoded preview and a real Reopen with Encoding' {
    $d = New-TestEnv $DataRoot 'ro'
    $cyr = [Text.Encoding]::GetEncoding(1251).GetBytes([string]::Join('', @([char]0x41F, [char]0x440, [char]0x438, [char]0x432, [char]0x435, [char]0x442)) + " mir`n")
    [IO.File]::WriteAllBytes("$d\files\ru.txt", $cyr)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\ru.txt") -Tag 'ro'
    $before = (Get-State $app).state.text_head
    Shot $app 'pp-ro-0-before'
    $want = [string]::Join('', @([char]0x41F, [char]0x440, [char]0x438, [char]0x432, [char]0x435, [char]0x442)) + " mir"
    # re-decode as Windows-1252 first: the text on screen must change to mojibake (proves the file is re-read, not just a label)
    Palette $app 'Reopen with Encoding'
    Send-Text $app 'windows-1252'
    Start-Sleep -Milliseconds 300
    Shot $app 'pp-ro-1a-picker-1252'
    Send-Chord $app 'enter' 700
    $mojibake = (Get-State $app).state.text_head
    if ($mojibake.StartsWith($want)) { throw "reopening as Windows-1252 left the Cyrillic text: [$mojibake]" }
    Palette $app 'Reopen with Encoding'
    Send-Text $app 'windows-1251'
    Start-Sleep -Milliseconds 400
    Shot $app 'pp-ro-1-picker-preview'
    Send-Chord $app 'enter' 700
    $after = (Get-State $app).state
    Shot $app 'pp-ro-2-after'
    if (-not $after.text_head.StartsWith($want)) { throw "after reopen as 1251 the text is [$($after.text_head)], want '$want'" }
    if ($after.dirty) { throw 'the document is dirty after a reopen' }
    "reopen 1252 -> [$($mojibake.Length) chars, differs], reopen 1251 -> Cyrillic restored, dirty=$($after.dirty)"
} }

if (Want 'project') { Invoke-Scenario 'project' 'Project file: Save Project writes <settings>/projects, --project reopens folder + files + caret' {
    $d = New-TestEnv $DataRoot 'pj'
    $proj = "$d\proj"
    New-Item -ItemType Directory -Force $proj | Out-Null
    [IO.File]::WriteAllText("$proj\one.txt", ((1..30 | ForEach-Object { "one $_" }) -join "`n"), $u)
    [IO.File]::WriteAllText("$proj\two.txt", ((1..30 | ForEach-Object { "two $_" }) -join "`n"), $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Extra @('--new-window', '--no-session', '--folder', $proj) -Files @("$proj\one.txt", "$proj\two.txt") -Tag 'pj'
    Send-Chord $app 'ctrl+g' 300; Send-Text $app '12'; Send-Chord $app 'enter' 400      # caret of the active tab (two.txt)
    Palette $app 'Save Project'
    Start-Sleep -Milliseconds 500
    Shot $app 'pp-pj-0-saved'
    Stop-AppGraceful $app | Out-Null
    $pf = Get-ChildItem "$d\settings\projects" -Filter '*.brouillon-project.toml' -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $pf) { throw 'no project file written under <settings>/projects' }
    $inside = @(Get-ChildItem $proj -Recurse -Filter '*project*' -ErrorAction SilentlyContinue).Count
    if ($inside -ne 0) { throw 'a project file was written inside the project folder' }
    $txt = [IO.File]::ReadAllText($pf.FullName)
    if ($txt -notmatch 'two.txt' -or $txt -notmatch 'line = 12') { throw "project file content: $txt" }
    # reopen from the file only
    $app2 = Launch-Brouillon -Exe $Exe -Dir $d -Extra @('--no-session', '--project', $pf.FullName) -Tag 'pj2'
    $s = (Get-State $app2).state
    Shot $app2 'pp-pj-1-reopened'
    $titles = ($s.tab_titles -join ',')
    if ($s.tab_count -ne 2 -or $s.tab_title -ne 'two.txt' -or (($s.caret -join ' ') -ne '12 1')) { throw "reopened: tabs=$titles active=$($s.tab_title) caret=$($s.caret -join ' ')" }
    "saved $($pf.Name) outside the folder; --project reopened tabs=$titles active=$($s.tab_title) caret=$($s.caret -join ' ')"
} }

if (Want 'mcp-default') { Invoke-Scenario 'mcp-default' 'MCP link is OFF by default: Preferences > Connect' {
    $d = New-TestEnv $DataRoot 'mcp'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'mcp'
    Shot $app 'pp-mcp-0-start'
    Send-Chord $app 'ctrl+,' 700
    Shot $app 'pp-mcp-1-prefs'
    $st = Dbg $app 'link|mcp'
    "link lines: $($st -join ' | ')"
} }

if (Want 'docends') { Invoke-Scenario 'docends' 'Ctrl+End / Ctrl+Home move to the document end / start' {
    $d = New-TestEnv $DataRoot 'de'
    $t = (1..60 | ForEach-Object { "line $_" }) -join "`n"
    [IO.File]::WriteAllText("$d\files\n.txt", $t, $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\n.txt") -Tag 'de'
    Send-Chord $app 'ctrl+end' 400
    $a = ((Get-State $app).state.caret -join ' ')
    Send-Chord $app 'ctrl+home' 400
    $b = ((Get-State $app).state.caret -join ' ')
    Shot $app 'pp-de-0'
    if ($a -ne '60 8' -or $b -ne '1 1') { throw "ctrl+end -> [$a] (want 60 8), ctrl+home -> [$b] (want 1 1)" }
    "ctrl+end=[$a] ctrl+home=[$b]"
} }

if (Want 'mcp-connect') { Invoke-Scenario 'mcp-connect' 'Connect tab: layout, switch off by default' {
    $d = New-TestEnv $DataRoot 'mcpc'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'mcpc'
    Send-Chord $app 'ctrl+,' 700
    Px $app 1563 159
    Start-Sleep -Milliseconds 500
    Shot $app 'pp-mcpc-0-connect'
    $link = Test-Path "$d\settings\link"
    "link folder before enabling: $link"
} }

if (Want 'autocomplete') { Invoke-Scenario 'autocomplete' 'Autocomplete popup from buffer words' {
    $d = New-TestEnv $DataRoot 'ac'
    [IO.File]::WriteAllText("$d\files\a.txt", "elephant`nelevator`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'ac'
    Send-Chord $app 'ctrl+end' 300
    Send-Chord $app 'enter' 200
    Send-Text $app 'ele'
    Start-Sleep -Milliseconds 700
    Shot $app 'pp-ac-0-typed-ele'
    Send-Chord $app 'tab' 400
    $s = Get-State $app
    Shot $app 'pp-ac-1-after-tab'
    "head=[$($s.state.text_head -replace "`n",'|')]"
} }

if (Want 'shortcut-record') { Invoke-Scenario 'shortcut-record' 'Shortcut editor: record Ctrl+J for Duplicate Line, keymap.toml, use it' {
    $d = New-TestEnv $DataRoot 'sr'
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`nbeta`n", $u)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt") -Tag 'sr'
    Send-Chord $app 'ctrl+k' 250
    Send-Text $app 'Keyboard Shortcuts'
    Send-Chord $app 'enter' 900
    Send-Text $app 'duplicate line'
    Start-Sleep -Milliseconds 400
    Px $app 1384 311            # Set... on the first row
    Shot $app 'pp-sr-0-recording'
    Send-Chord $app 'ctrl+j' 500
    Shot $app 'pp-sr-1-bound'
    Send-Chord $app 'esc' 400
    $k = if (Test-Path "$d\settings\keymap.toml") { [IO.File]::ReadAllText("$d\settings\keymap.toml") } else { '' }
    if ($k -notmatch 'ctrl\+j' -or $k -notmatch 'duplicate_line') { throw "keymap.toml: [$k]" }
    Send-Chord $app 'ctrl+j' 400
    Send-Chord $app 'ctrl+s' 700
    $t = [IO.File]::ReadAllText("$d\files\a.txt")
    if ($t -ne "alpha`nalpha`nbeta`n") { throw "Ctrl+J did not duplicate: $($t -replace "`n",'|')" }
    'recorded, persisted in keymap.toml and used (file duplicated line 1)'
} }

$script:Results
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize | Out-String -Width 300 | Write-Host
