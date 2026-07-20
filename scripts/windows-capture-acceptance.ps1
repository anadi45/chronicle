param(
    [string]$Executable = "$PSScriptRoot\..\src-tauri\target\release\chronicle.exe",
    [string]$Database = "$PSScriptRoot\..\chronicle.db",
    [int]$ObservationSeconds = 8
)

$ErrorActionPreference = "Stop"
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable -ErrorAction Stop).Path
$resolvedDatabase = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Database))
$before = if (Test-Path -LiteralPath $resolvedDatabase) { (Get-Item $resolvedDatabase).LastWriteTimeUtc } else { [DateTime]::MinValue }
$chronicle = Start-Process -FilePath $resolvedExecutable -WorkingDirectory (Split-Path $resolvedDatabase) -PassThru
$foreground = Start-Process -FilePath "notepad.exe" -PassThru
try {
    Start-Sleep -Seconds $ObservationSeconds
    $chronicle.Refresh()
    if ($chronicle.HasExited) { throw "Chronicle exited with code $($chronicle.ExitCode)" }
    if (-not (Test-Path -LiteralPath $resolvedDatabase)) { throw "Chronicle did not create $resolvedDatabase" }

    $python = @'
import sqlite3, sys
db, before = sys.argv[1], sys.argv[2]
connection = sqlite3.connect(db)
row = connection.execute("SELECT COUNT(*) FROM raw_events WHERE source = 'foreground_window'").fetchone()
print(row[0])
'@
    $count = ($python | python - $resolvedDatabase $before).Trim()
    if ([int]$count -eq 0) {
        Write-Warning "No foreground events were recorded. Verify Capture is enabled in Settings and rerun."
        exit 2
    }
    Write-Host "Windows capture acceptance passed: $count foreground events found."
}
finally {
    if (-not $foreground.HasExited) { Stop-Process -Id $foreground.Id -Force }
    if (-not $chronicle.HasExited) { Stop-Process -Id $chronicle.Id -Force }
    $foreground.Dispose(); $chronicle.Dispose()
}
