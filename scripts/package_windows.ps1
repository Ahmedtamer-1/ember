# Ember Browser — Windows Portable Packager & Installer Generator
param(
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"

$releaseExe = "target\release\ember.exe"
$oldExe = "target\release\ember.exe.old"
if (Test-Path $oldExe) {
    Remove-Item $oldExe -Force -ErrorAction SilentlyContinue
}
if (Test-Path $releaseExe) {
    try {
        Remove-Item $releaseExe -Force -ErrorAction Stop
    } catch {
        $backup = if (-not (Test-Path $oldExe)) {
            $oldExe
        } else {
            "target\release\ember.$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()).old"
        }
        Write-Host "Existing ember.exe is locked; renaming it to $backup..."
        Move-Item -Path $releaseExe -Destination $backup -Force
    }
}

Write-Host "Building Ember release binary..."
cargo build --release
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$distDir = "target\dist"
$pkgName = "ember-v$Version-windows-x64"
$pkgDir = Join-Path $distDir $pkgName

if (Test-Path $pkgDir) {
    Remove-Item -Path $pkgDir -Recurse -Force
}
New-Item -ItemType Directory -Path $pkgDir -Force | Out-Null

Write-Host "Copying distribution artifacts..."
Copy-Item "target\release\ember.exe" -Destination $pkgDir
if (Test-Path "target\release\WebView2Loader.dll") {
    Copy-Item "target\release\WebView2Loader.dll" -Destination $pkgDir
}
Copy-Item "README.md" -Destination $pkgDir -ErrorAction SilentlyContinue
Copy-Item "BENCHMARKS.md" -Destination $pkgDir -ErrorAction SilentlyContinue
Copy-Item "LICENSE-MIT" -Destination $pkgDir -ErrorAction SilentlyContinue
Copy-Item "LICENSE-APACHE" -Destination $pkgDir -ErrorAction SilentlyContinue
if (Test-Path "assets") {
    Copy-Item "assets" -Destination $pkgDir -Recurse -Force -ErrorAction SilentlyContinue
}

# Write a simple one-click installer script inside the package
$installScript = @"
`$ErrorActionPreference = "Stop"
`$installDir = "`$env:LOCALAPPDATA\Ember"
Write-Host "Installing Ember to `$installDir..."
New-Item -ItemType Directory -Path `$installDir -Force | Out-Null
Copy-Item "ember.exe" -Destination `$installDir -Force
if (Test-Path "WebView2Loader.dll") {
    Copy-Item "WebView2Loader.dll" -Destination `$installDir -Force
}

# Create Desktop and Start Menu Shortcuts
`$wsh = New-Object -ComObject WScript.Shell
`$desktopShortcut = `$wsh.CreateShortcut("`$env:USERPROFILE\Desktop\Ember.lnk")
`$desktopShortcut.TargetPath = "`$installDir\ember.exe"
`$desktopShortcut.Description = "Ember Web Browser"
`$desktopShortcut.Save()

`$startMenuDir = "`$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
`$startShortcut = `$wsh.CreateShortcut("`$startMenuDir\Ember.lnk")
`$startShortcut.TargetPath = "`$installDir\ember.exe"
`$startShortcut.Description = "Ember Web Browser"
`$startShortcut.Save()

Write-Host "Installation successful! Shortcuts created on Desktop and Start Menu."
"@
Set-Content -Path (Join-Path $pkgDir "install.ps1") -Value $installScript

# Create Portable ZIP archive
$zipPath = Join-Path $distDir "$pkgName.zip"
if (Test-Path $zipPath) {
    Remove-Item $zipPath -Force
}
Write-Host "Creating portable package: $zipPath"
Compress-Archive -Path "$pkgDir\*" -DestinationPath $zipPath -Force

Write-Host "`nWindows package created successfully at: $zipPath"
$sizeMB = [math]::Round((Get-Item $zipPath).Length / 1MB, 2)
Write-Host "Package Size: $sizeMB MB"
