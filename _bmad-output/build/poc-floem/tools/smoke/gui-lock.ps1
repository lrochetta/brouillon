<#
  Cross-agent desktop lock: only one process may drive the GUI at a time.
  Enter-GuiLock creates D:\travail\_gui.lock exclusively (retry up to 20 min); Exit-GuiLock deletes it.
  Always call Exit-GuiLock in a finally block.
#>
$script:GuiLockPath = 'D:\travail\_gui.lock'
$script:GuiLockHeld = $false

function Enter-GuiLock {
    param([int]$TimeoutSec = 1200)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        try {
            $fs = [IO.File]::Open($script:GuiLockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $b = [Text.Encoding]::UTF8.GetBytes("pid=$PID phase3 $(Get-Date -Format o)")
            $fs.Write($b, 0, $b.Length); $fs.Close()
            $script:GuiLockHeld = $true
            return
        } catch [System.IO.IOException] {
            if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) { throw "GUI_LOCK_TIMEOUT after $TimeoutSec s" }
            Start-Sleep -Seconds 5
        }
    }
}

function Exit-GuiLock {
    if ($script:GuiLockHeld) {
        Remove-Item -Force $script:GuiLockPath -ErrorAction SilentlyContinue
        $script:GuiLockHeld = $false
    }
}
