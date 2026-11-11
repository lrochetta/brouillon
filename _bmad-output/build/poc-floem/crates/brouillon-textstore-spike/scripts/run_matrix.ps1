# Generates the test files, runs bench.exe for rope|mmap|chunked on each, appends to $Out, deletes the files.
# Usage: pwsh run_matrix.ps1 -Target D:\travail\_t-store -Data D:\travail\_t-store-data
# Never more than ~2 GB of temp files: a 1 GB file + the save's temp file.
param(
    [string]$Target = 'D:\travail\_t-store',
    [string]$Data = 'D:\travail\_t-store-data',
    [string[]]$Sizes = @('1', '10', '100', '1000000000'),
    [string]$Out = ''
)
if (-not $Out) { $Out = Join-Path $Data 'results.txt' }
New-Item -ItemType Directory -Force $Data | Out-Null
$gen = Join-Path $Target 'release\examples\genfile.exe'
$bench = Join-Path $Target 'release\examples\bench.exe'
foreach ($size in $Sizes) {
    $combos = @(@('code', 'lf'), @('code', 'crlf'), @('prose', 'lf'), @('prose', 'crlf'))
    if ($size -eq '1000000000') { $combos = @(@('code', 'lf'), @('prose', 'crlf')) }  # 1 GB: two representative files
    foreach ($c in $combos) {
        $name = "s$($size)_$($c[0])_$($c[1]).txt"
        $f = Join-Path $Data $name
        & $gen $f $size $c[0] $c[1] | Out-Null
        foreach ($store in @('rope', 'mmap', 'chunked')) {
            Write-Host "== $store $name"
            & $bench $store $f 10000 | Tee-Object -FilePath $Out -Append | Out-Null
            # fresh copy of the original bytes (the bench saves over the file)
            & $gen $f $size $c[0] $c[1] | Out-Null
        }
        foreach ($store in @('mmap', 'chunked', 'rope')) {
            $env:EARLY = '1'
            & $bench $store $f | Tee-Object -FilePath $Out -Append | Out-Null
            Remove-Item Env:\EARLY
        }
        Remove-Item $f -Force
    }
}
Write-Host 'done'
