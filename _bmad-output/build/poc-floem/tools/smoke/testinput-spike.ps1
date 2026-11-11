<#
.SYNOPSIS
  Proof of the test-input channel: with the window NOT focused (off-screen, never activated) and the real mouse and
  keyboard untouched, (1) typed text reaches the editor, (2) Ctrl+K opens the palette, (3) a click on a tab switches it.
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\smoke\testinput-spike.ps1 -Exe <exe built with --features test-input>
  Records the foreground window before and after: it must not change.
#>
param([string]$Exe, [string]$DataRoot = (Join-Path $env:TEMP 'brouillon-ti-spike'))
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'testinput.ps1')
$Exe = (Resolve-Path $Exe).Path
$env:BROUILLON_NO_UPDATE = '1'
New-Item -ItemType Directory -Force $DataRoot | Out-Null

Invoke-Scenario 'ti1' 'test-input spike: text, Ctrl+K, tab click, no focus taken' {
    $d = New-TestEnv $DataRoot 'spike'
    [IO.File]::WriteAllText("$d\settings\settings.toml", "[firstrun]`ndone = true`n")
    [IO.File]::WriteAllText("$d\files\a.txt", "alpha`n")
    [IO.File]::WriteAllText("$d\files\b.txt", "beta`n")
    $fgBefore = [W32]::GetForegroundWindow().ToInt64()
    $cursorBefore = New-Object TIPOINT; [void][TI32]::GetCursorPos([ref]$cursorBefore)
    $app = Launch-Brouillon -Exe $Exe -Dir $d -Files @("$d\files\a.txt", "$d\files\b.txt") -Tag 'spike'
    $s0 = Get-State $app
    $out = @("start: tab=$($s0.state.tab_title) tabs=$($s0.state.tab_count) focus=[$($s0.state.focus)]")
    # 1. text reaches the editor
    Send-Text $app 'hello world'
    $s1 = Wait-State $app { param($s) $s.state.text_head -like '*hello world*' }
    $out += "text: head='$($s1.state.text_head.Trim())' caret=$($s1.state.caret -join ',')"
    # 2. Ctrl+K opens the palette
    Send-Chord $app 'ctrl+k'
    $s2 = Wait-State $app { param($s) $s.state.palette_open }
    $out += "ctrl+k: palette_open=$($s2.state.palette_open)"
    Send-Chord $app 'esc'
    [void](Wait-State $app { param($s) -not $s.state.palette_open })
    Save-Shot $app 'ti-spike-1' | Out-Null
    # 3. a click on a tab switches it: the tab bar is at the top of the client area; scan along it for the other tab
    $before = (Get-State $app).state.active_tab
    $switched = $false
    foreach ($x in 30, 60, 90, 120, 150, 180, 220, 260) {
        [void](Invoke-TI $app @{ op = 'click'; x = $x; y = 64 })
        Start-Sleep -Milliseconds 200
        if ((Get-State $app).state.active_tab -ne $before) { $switched = $true; $out += "tab click at x=$x switched $before -> $((Get-State $app).state.active_tab)"; break }
    }
    if (-not $switched) { $out += 'tab click: no switch found (adjust y/x)' }
    $fgAfter = [W32]::GetForegroundWindow().ToInt64()
    $cursorAfter = New-Object TIPOINT; [void][TI32]::GetCursorPos([ref]$cursorAfter)
    $out += "foreground unchanged=$($fgBefore -eq $fgAfter) cursor unchanged=$($cursorBefore.X -eq $cursorAfter.X -and $cursorBefore.Y -eq $cursorAfter.Y) appIsForeground=$((Get-ForegroundPid) -eq $app.Proc.Id)"
    $out -join ' | '
}
$script:Results | Format-Table -AutoSize -Wrap
