# Release zip: Tobasu.exe + ffmpeg + player/config + mpv + license files.
# Usage: .\scripts\package.ps1 -Mpv C:\path\to\mpv-folder -Ffmpeg C:\path\to\ffmpeg-win64-gpl-shared
param([Parameter(Mandatory = $true)][string]$Mpv, [Parameter(Mandatory = $true)][string]$Ffmpeg)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# cargo ilerlemeyi stderr'e yazar; Windows PowerShell bunu hata sanmasın
$ErrorActionPreference = "Continue"
cmd /c "cargo build --release 2>&1" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$ErrorActionPreference = "Stop"

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "Tobasu-$version-windows-x64"
$out = Join-Path $root "dist\$name"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force "$out\player" | Out-Null

Copy-Item target\release\Tobasu.exe $out
Copy-Item -Recurse player\config "$out\player\config"
Remove-Item -Force -ErrorAction SilentlyContinue "$out\player\config\hiz.txt"
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "$out\player\config\watch_later"
New-Item -ItemType Directory -Force "$out\player\mpv" | Out-Null
foreach ($f in "mpv.exe", "mpv.com", "d3dcompiler_43.dll") { Copy-Item (Join-Path $Mpv $f) "$out\player\mpv" }
if (Test-Path (Join-Path $Mpv "doc")) { Copy-Item -Recurse (Join-Path $Mpv "doc") "$out\player\mpv\doc" }
Copy-Item scripts\mpv-LICENSE.txt "$out\player\mpv\LICENSE.txt"
New-Item -ItemType Directory -Force "$out\ffmpeg" | Out-Null
Get-ChildItem (Join-Path $Ffmpeg "bin") | Where-Object { $_.Name -ne "ffplay.exe" } | Copy-Item -Destination "$out\ffmpeg"
Copy-Item (Join-Path $Ffmpeg "LICENSE.txt") "$out\ffmpeg\LICENSE.txt"
Copy-Item LICENSE, THIRD-PARTY.md, README.md $out

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
# Compress-Archive (PowerShell 5.1) yollara ters eğik çizgi yazar; Windows'un tar'ı standart zip üretir
Push-Location $out
tar.exe -a -c -f $zip *
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "zip failed" }
Get-Item $zip | Select-Object Name, @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } }
