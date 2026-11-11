<#
.SYNOPSIS
  Dark / light mode screenshots (theme-<mode>-*.png) with the smoke harness.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\theme.ps1 -Exe C:\path\brouillon.exe [-Mode light,dark] [-Out dir]

  Same safety contract as run.ps1: keys and clicks are sent only while the foreground
  window belongs to the launched process (otherwise the run is ABORTED(focus)); the
  process is always killed. The app runs with a TEMPORARY --settings-dir whose
  settings.toml pins theme.mode. Do not touch keyboard/mouse while it runs (~1 minute).
#>
param(
    [string]$Exe,
    [string[]]$Mode = @('light', 'dark'),
    [string]$Out = '',
    [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-theme')
)
$ErrorActionPreference = 'Stop'
$Mode = @($Mode | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'lib.ps1')
if (-not $Exe -or -not (Test-Path $Exe)) { throw 'pass -Exe <path to brouillon.exe>' }
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $DataRoot | Out-Null
function Write-Utf8([string]$p, [string]$s) { [IO.File]::WriteAllText($p, $s, (New-Object Text.UTF8Encoding($false))) }

# LOGICAL px from the window's outer top-left (1100x720 logical window).
$MENU_VIEW = @(200, 63); $BADGE = @(1069, 39); $CRAYON = @(617, 91); $TAB_MD = @(170, 98); $EDITOR_TEXT = @(600, 136)   # y -24 since f694fd3 (toolbar row removed); $CRAYON is STALE (button moved to the header), x unknown

$code = @"
// Theme check: keywords, strings, numbers, comments, types.
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Point { x: i32, y: i32 }

/// Adds two points.
fn add(a: &Point, b: &Point) -> Point {
    let label = "sum"; // a trailing comment
    let n: u64 = 0xFF + 42;
    if n > 10 && label.len() == 3 { println!("{label} {n}"); }
    Point { x: a.x + b.x, y: a.y + b.y }
}

fn main() {
    let mut m: HashMap<String, i32> = HashMap::new();
    m.insert("a".to_string(), 1);
    let p = add(&Point { x: 1, y: 2 }, &Point { x: 3, y: 4 });
    println!("{:?}", p);
}
"@
$md = @"
# Notes

Plain **bold** and *italic* text with `inline code` and a [link](https://example.org).

- first item
- second item

> a quote

``````rust
fn main() {}
``````
"@

foreach ($m in $Mode) {
    Invoke-Scenario "theme-$m" "screenshots in $m mode" {
        $d = New-TestEnv $DataRoot $m
        Write-Utf8 "$d\files\code.rs" $code
        Write-Utf8 "$d\files\notes.md" $md
        Write-Utf8 "$d\settings\settings.toml" "[theme]`nmode = `"$m`"`n"
        $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\code.rs", "$d\files\notes.md") -Tag "theme-$m"
        $dest = if ($Out) { $Out } else { Get-SmokeOut }
        New-Item -ItemType Directory -Force $dest | Out-Null
        function Shot([string]$n) { Write-Host "  shot $n"; $p = Save-Shot $app "theme-$m-$n"; if ($p -and $dest -ne (Split-Path $p)) { Copy-Item $p (Join-Path $dest "theme-$m-$n.png") -Force } }
        Send-Chord $app 'ctrl+pgup'                      # back to code.rs
        Shot '1-code'
        Send-Click $app $EDITOR_TEXT[0] $EDITOR_TEXT[1]; Send-Text $app 'x'
        Shot '2-modified-tab'
        Send-Chord $app 'ctrl+f'; Send-Text $app 'let'
        Shot '3-search'
        Send-Chord $app 'ctrl+f'                         # Esc does not close the panel; Ctrl+F toggles it
        Send-Click $app $EDITOR_TEXT[0] $EDITOR_TEXT[1]
        Send-Click $app $MENU_VIEW[0] $MENU_VIEW[1]
        Shot '4-menu-view'
        Send-Click $app $MENU_VIEW[0] $MENU_VIEW[1]     # toggles the menu closed
        Send-Click $app $EDITOR_TEXT[0] $EDITOR_TEXT[1]
        Send-Click $app $BADGE[0] $BADGE[1]
        Shot '6-badge-tooltip'
        Send-Click $app $BADGE[0] $BADGE[1]
        Send-Click $app $TAB_MD[0] $TAB_MD[1]            # notes.md
        Send-Chord $app 'ctrl+shift+v'
        Shot '7-markdown'
        Send-Chord $app 'ctrl+shift+v'
        Send-Click $app $CRAYON[0] $CRAYON[1]
        Shot '8-crayon-toolbar'
        Send-Click $app $CRAYON[0] $CRAYON[1]
        Send-Chord $app 'ctrl+k'; Send-Text $app 'theme'  # last: neither Esc nor Ctrl+K closes the palette
        Shot '5-palette'
        "done ($dest)"
    }
}
# Live switch through the View menu, persisted to settings.toml (rows at y = 98 + 24 * index logical).
Invoke-Scenario 'theme-switch' 'View > Theme: Light / Dark / Follow System, live and saved' {
    $d = New-TestEnv $DataRoot 'switch'
    Write-Utf8 "$d\files\code.rs" $code
    Write-Utf8 "$d\settings\settings.toml" "[theme]`nmode = `"dark`"`n"
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\code.rs") -Tag 'theme-switch'
    $dest = if ($Out) { $Out } else { Get-SmokeOut }
    New-Item -ItemType Directory -Force $dest | Out-Null
    function Pick([int]$row, [string]$shot) {
        Send-Click $app $MENU_VIEW[0] $MENU_VIEW[1]
        Send-Click $app 217 (98 + 24 * $row)
        $p = Save-Shot $app $shot
        if ($p -and $dest -ne (Split-Path $p)) { Copy-Item $p (Join-Path $dest "$shot.png") -Force }
        (Get-Content -Raw "$d\settings\settings.toml") -replace "`r?`n", ' '
    }
    $a = Pick 3 'theme-switch-1-light-live'
    $b = Pick 2 'theme-switch-2-dark-live'
    $c = Pick 4 'theme-switch-3-follow-system'
    $os = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize').AppsUseLightTheme
    "after Light: [$a] after Dark: [$b] after System: [$c]; OS AppsUseLightTheme=$os"
}
$bad = @($script:Results | Where-Object { $_.Status -ne 'OK' })
if ($bad.Count) { exit 1 }
