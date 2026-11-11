<#
.SYNOPSIS
  Ctrl+P quick open and keyboard capture, dark and light (qo-<mode>-*.png).
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\quick-open.ps1 -Exe C:\path\brouillon.exe [-Mode dark,light] [-Tag x-]
  Same safety contract as run.ps1 (keys only while our window is the foreground one; TEMPORARY
  --settings-dir; BROUILLON_NO_UPDATE=1; the process is always killed). Captures land in tools\smoke\out\.
  Proofs that nothing leaks into the document: files are saved (Ctrl+S) and read back byte for byte.
  The recent-files list lives in the real ~/.brouillon/recent.toml (not isolated by --settings-dir):
  it is backed up, pinned to an empty list for the run and restored afterwards.
#>
param([string]$Exe, [string[]]$Mode = @('dark', 'light'), [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-qo'), [string]$Tag = '')
$ErrorActionPreference = 'Stop'
$Mode = @($Mode | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'lib.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null
$u = New-Object Text.UTF8Encoding($false)
function Shot($app, $name) { Save-Shot $app "$Tag$name" | Out-Null }
function Same($path, $expected) { [IO.File]::ReadAllText($path) -ceq $expected }

$recent = Join-Path $env:USERPROFILE '.brouillon\recent.toml'
$backup = if (Test-Path $recent) { [IO.File]::ReadAllBytes($recent) } else { $null }
try {
    if (Test-Path $recent) { Remove-Item $recent }
    foreach ($m in $Mode) {
        Invoke-Scenario "qo-$m" "Ctrl+P quick open in $m mode" {
            $d = New-TestEnv $DataRoot $m
            [IO.File]::WriteAllText("$d\settings\settings.toml", "[theme]`nmode = `"$m`"`n", $u)
            $lines = (1..12 | ForEach-Object { "line $_ of the main file" }) -join "`n"
            $main = $lines + "`n"
            [IO.File]::WriteAllText("$d\files\main.txt", $main, $u)
            $alpha = "ALPHA-CONTENT`n"; $beta = "BETA-CONTENT`n"
            [IO.File]::WriteAllText("$d\files\alpha.txt", $alpha, $u)
            [IO.File]::WriteAllText("$d\files\beta_notes.md", $beta, $u)
            $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\main.txt") -Tag "qo-$m"
            Shot $app "qo-$m-0-start"

            # 1. Ctrl+P opens the panel.
            Send-Chord $app 'ctrl+p' 500
            Shot $app "qo-$m-1-open"
            # 2. ':5' + Enter goes to line 5; nothing is typed into the document.
            Send-Text $app ':5'
            Shot $app "qo-$m-2-colon5"
            Send-Chord $app 'enter' 400
            Shot $app "qo-$m-3-line5"
            Send-Chord $app 'ctrl+s' 400
            if (-not (Same "$d\files\main.txt" $main)) { throw 'LEAK: main.txt changed after ":5" + Enter' }
            # 3. File text opens the right file in a tab.
            Send-Chord $app 'ctrl+p' 500
            Send-Text $app 'beta'
            Shot $app "qo-$m-4-file-query"
            Send-Chord $app 'enter' 500
            Shot $app "qo-$m-5-beta-opened"
            # 4. Escape closes and the focus is back in the editor.
            Send-Chord $app 'ctrl+p' 500
            Send-Text $app 'typed-while-open'
            Send-Chord $app 'esc' 400
            Save-Shot $app "${Tag}qo-$m-6-escaped" 1500 | Out-Null
            Send-Text $app 'Q'; Save-Shot $app "${Tag}qo-$m-6b-typed" 1500 | Out-Null
            Send-Chord $app 'ctrl+s' 400
            $now = [IO.File]::ReadAllText("$d\files\beta_notes.md")
            if ($now -like '*typed-while-open*') { throw 'LEAK: text typed in the open panel reached beta_notes.md' }
            if ($now -cnotlike '*Q*') { throw 'FOCUS: the editor did not get the keyboard back after Escape' }
            # 5. The command palette captures the keyboard too.
            Send-Chord $app 'ctrl+k' 500
            Send-Text $app 'zzq'
            Shot $app "qo-$m-7-palette"
            Send-Chord $app 'esc' 400
            Save-Shot $app "${Tag}qo-$m-8-palette-closed" 1500 | Out-Null
            Send-Chord $app 'ctrl+s' 400
            if ([IO.File]::ReadAllText("$d\files\beta_notes.md") -like '*zzq*') { throw 'LEAK: text typed in the command palette reached the document' }
            # 6. Arrow keys + Enter pick a row; a mouse click picks one too (recent files come first).
            Send-Chord $app 'ctrl+p' 500
            Send-Chord $app 'down' 150; Send-Chord $app 'down' 150
            Save-Shot $app "${Tag}qo-$m-9-arrows" 800 | Out-Null
            Send-Chord $app 'enter' 700
            Save-Shot $app "${Tag}qo-$m-10-arrow-opened" 800 | Out-Null
            Send-Chord $app 'ctrl+p' 500
            Send-Text $app 'main'
            Save-Shot $app "${Tag}qo-$m-11-before-click" 800 | Out-Null
            Send-Click $app 400 170                      # first result row (logical px)
            Save-Shot $app "${Tag}qo-$m-12-clicked" 1000 | Out-Null
            if (-not (Same "$d\files\alpha.txt" $alpha)) { throw 'alpha.txt changed' }
            Send-Chord $app 'ctrl+s' 400
            if (-not (Same "$d\files\main.txt" $main)) { throw 'LEAK: main.txt changed' }
            'no leak into main.txt / beta_notes.md; Escape returns the focus to the editor'
        }
    }
} finally {
    if ($null -ne $backup) { New-Item -ItemType Directory -Force (Split-Path $recent) | Out-Null; [IO.File]::WriteAllBytes($recent, $backup) }
    elseif (Test-Path $recent) { Remove-Item $recent }
}
$script:Results | Format-Table Id, Status, Seconds, Detail -AutoSize -Wrap | Out-String -Width 250 | Write-Host
Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
