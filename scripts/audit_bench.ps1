# Ember vs. Chrome Performance & RAM Audit Script

param(
    [switch]$Quick
)

$ErrorActionPreference = "Stop"

$emberExe = (Resolve-Path "target\release\ember.exe").Path
$chromeExe = "C:\Program Files\Google\Chrome\Application\chrome.exe"
$hasChrome = Test-Path $chromeExe

Write-Host "================================================="
Write-Host " Ember Performance, Size & RAM Benchmark Suite"
Write-Host "================================================="
Write-Host "Ember Binary:  $emberExe"
Write-Host "Chrome Binary: $chromeExe (Available: $hasChrome)"

# Binary Size
$emberSize = (Get-Item $emberExe).Length
$emberSizeMB = [math]::Round($emberSize / 1MB, 2)
Write-Host "Ember Binary Size: $emberSize bytes ($emberSizeMB MB)"

if ($hasChrome) {
    $chromeSize = (Get-Item $chromeExe).Length
    $chromeSizeMB = [math]::Round($chromeSize / 1MB, 2)
    Write-Host "Chrome Binary Size (stub): $chromeSize bytes ($chromeSizeMB MB)"
}

# 1, 5, and 20 tab URL lists
$urls1 = @("https://example.com")
$urls5 = @(
    "https://example.com",
    "https://news.ycombinator.com",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://github.com",
    "https://duckduckgo.com"
)
$urls20 = @(
    "https://example.com",
    "https://news.ycombinator.com",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://github.com",
    "https://duckduckgo.com",
    "https://www.rust-lang.org",
    "https://crates.io",
    "https://doc.rust-lang.org/book/",
    "https://developer.mozilla.org/en-US/",
    "https://httpbin.org",
    "https://en.wikipedia.org/wiki/Web_browser",
    "https://en.wikipedia.org/wiki/Ad_blocking",
    "https://news.ycombinator.com/news",
    "https://github.com/explore",
    "https://en.wikipedia.org/wiki/Windows_11",
    "https://example.org",
    "https://example.net",
    "https://www.wikipedia.org",
    "https://startpage.com",
    "https://html5test.co"
)

function Get-ProcessTreeRamMB([int]$rootPid, [string]$processNamePattern) {
    $pids = @($rootPid)
    $added = $true
    while ($added) {
        $added = $false
        $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
        foreach ($p in $all) {
            if ($pids -contains $p.ParentProcessId -and -not ($pids -contains $p.ProcessId)) {
                $pids += $p.ProcessId
                $added = $true
            }
        }
    }
    
    # Also find any associated processes matching the browser engine
    if ($processNamePattern) {
        $extra = Get-Process | Where-Object { $_.Name -like "*$processNamePattern*" } | Select-Object -ExpandProperty Id
        foreach ($id in $extra) {
            if (-not ($pids -contains $id)) {
                # Check if started recently / part of our test tree
                $proc = Get-Process -Id $id -ErrorAction SilentlyContinue
                if ($proc) {
                    $pids += $id
                }
            }
        }
    }

    $totalBytes = 0
    foreach ($id in $pids) {
        $proc = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($proc) {
            $totalBytes += $proc.WorkingSet64
        }
    }
    return [math]::Round($totalBytes / 1MB, 1)
}

function Measure-BrowserStartup([string]$exePath, [string]$argsStr) {
    $times = @()
    $iterations = if ($Quick) { 2 } else { 3 }
    for ($i = 0; $i -lt $iterations; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $p = if ([string]::IsNullOrWhiteSpace($argsStr)) {
            Start-Process -FilePath $exePath -PassThru
        } else {
            Start-Process -FilePath $exePath -ArgumentList $argsStr -PassThru
        }
        # Wait until process window appears or input is idle
        $null = $p.WaitForInputIdle(4000)
        $sw.Stop()
        $times += $sw.ElapsedMilliseconds
        Start-Sleep -Milliseconds 500
        $p.Kill()
        Start-Sleep -Milliseconds 400
    }
    $avg = ($times | Measure-Object -Average).Average
    return [math]::Round($avg, 0)
}

Write-Host "`nMeasuring Startup Times..."
$emberStartupMs = Measure-BrowserStartup $emberExe ""
Write-Host "Ember Cold/Warm Average Startup: $emberStartupMs ms"

$chromeStartupMs = "N/A"
if ($hasChrome) {
    $tempDir = Join-Path $env:TEMP ("chrome_bench_" + (Get-Random))
    $chromeStartupMs = Measure-BrowserStartup $chromeExe "--user-data-dir=`"$tempDir`" --no-first-run --no-default-browser-check about:blank"
    Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Chrome Average Startup: $chromeStartupMs ms"
}

Write-Host "`nMeasuring Working Set RAM..."

function Test-EmberRam([string[]]$urls, [bool]$materializeAll = $false) {
    if ($materializeAll) {
        $env:EMBER_BENCH_MATERIALIZE_ALL = "1"
    } else {
        Remove-Item Env:\EMBER_BENCH_MATERIALIZE_ALL -ErrorAction SilentlyContinue
    }
    $argsStr = ($urls -join " ")
    $p = if ([string]::IsNullOrWhiteSpace($argsStr)) {
        Start-Process -FilePath $emberExe -PassThru
    } else {
        Start-Process -FilePath $emberExe -ArgumentList $argsStr -PassThru
    }
    $settleSeconds = if ($urls.Count -gt 5) { 7 } else { 4 }
    Start-Sleep -Seconds $settleSeconds
    $ram = Get-ProcessTreeRamMB $p.Id "msedgewebview2"
    
    $p.Kill()
    Get-Process -Name "msedgewebview2" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item Env:\EMBER_BENCH_MATERIALIZE_ALL -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
    return $ram
}

function Test-ChromeRam([string[]]$urls) {
    if (-not $hasChrome) { return "N/A" }
    $tempDir = Join-Path $env:TEMP ("chrome_bench_" + (Get-Random))
    $urlArgs = ($urls -join " ")
    $argsStr = "--user-data-dir=`"$tempDir`" --no-first-run --no-default-browser-check --disable-background-networking $urlArgs"
    $p = Start-Process -FilePath $chromeExe -ArgumentList $argsStr -PassThru
    $settleSeconds = if ($urls.Count -gt 5) { 7 } else { 4 }
    Start-Sleep -Seconds $settleSeconds
    $ram = Get-ProcessTreeRamMB $p.Id "chrome"
    
    $p.Kill()
    Get-Process -Name "chrome" -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $chromeExe } | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
    return $ram
}

$ember1TabRam = Test-EmberRam $urls1 $false
Write-Host "Ember 1 Tab RAM: $ember1TabRam MB"

$chrome1TabRam = Test-ChromeRam $urls1
Write-Host "Chrome 1 Tab RAM: $chrome1TabRam MB"

$ember5TabLazyRam = Test-EmberRam $urls5 $false
Write-Host "Ember 5 Tabs (Lazy) RAM: $ember5TabLazyRam MB"

$ember5TabMatRam = Test-EmberRam $urls5 $true
Write-Host "Ember 5 Tabs (All Live) RAM: $ember5TabMatRam MB"

$chrome5TabRam = Test-ChromeRam $urls5
Write-Host "Chrome 5 Tabs RAM: $chrome5TabRam MB"

$ember20TabLazyRam = Test-EmberRam $urls20 $false
Write-Host "Ember 20 Tabs (Lazy) RAM: $ember20TabLazyRam MB"

$ember20TabMatRam = Test-EmberRam $urls20 $true
Write-Host "Ember 20 Tabs (All Live) RAM: $ember20TabMatRam MB"

$chrome20TabRam = Test-ChromeRam $urls20
Write-Host "Chrome 20 Tabs RAM: $chrome20TabRam MB"

Write-Host "`n=== AUDIT SUMMARY ==="
Write-Host "Metric                       | Ember (Ours) | Chrome       | Brave (Est./Ref.)"
Write-Host "-----------------------------|--------------|--------------|------------------"
Write-Host ("Binary Size (Disk Footprint) | {0,-12} | {1,-12} | {2,-12}" -f "$emberSizeMB MB", "$chromeSizeMB MB (Core)", "~120 MB (Full)")
Write-Host ("Cold Startup Time            | {0,-12} | {1,-12} | {2,-12}" -f "$emberStartupMs ms", "$chromeStartupMs ms", "~280 ms")
Write-Host ("RAM: 1 Tab (example.com)     | {0,-12} | {1,-12} | {2,-12}" -f "$ember1TabRam MB", "$chrome1TabRam MB", "~125 MB")
Write-Host ("RAM: 5 Tabs (Default/Lazy)   | {0,-12} | {1,-12} | {2,-12}" -f "$ember5TabLazyRam MB", "$chrome5TabRam MB", "~340 MB")
Write-Host ("RAM: 5 Tabs (All Live)       | {0,-12} | {1,-12} | {2,-12}" -f "$ember5TabMatRam MB", "$chrome5TabRam MB", "~340 MB")
Write-Host ("RAM: 20 Tabs (Default/Lazy)  | {0,-12} | {1,-12} | {2,-12}" -f "$ember20TabLazyRam MB", "$chrome20TabRam MB", "~880 MB")
Write-Host ("RAM: 20 Tabs (All Live)      | {0,-12} | {1,-12} | {2,-12}" -f "$ember20TabMatRam MB", "$chrome20TabRam MB", "~880 MB")
