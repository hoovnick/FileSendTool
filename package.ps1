# Build FST and assemble a self-contained dist\ folder.
#
#   .\package.ps1                      # finds cloudflared on PATH
#   .\package.ps1 -Cloudflared C:\path\to\cloudflared.exe
#
# The result is a folder you can zip and hand to someone: unzip, run fst.exe.
# No install, no accounts.

param(
    [string]$Cloudflared = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$dist = Join-Path $root "dist"

if (-not $SkipBuild) {
    Write-Host "building release..." -ForegroundColor Cyan
    Push-Location $root
    cargo build --release
    if ($LASTEXITCODE -ne 0) { Pop-Location; throw "cargo build failed" }
    Pop-Location
}

if ($Cloudflared -eq "") {
    $cmd = Get-Command cloudflared -ErrorAction SilentlyContinue
    if (-not $cmd) {
        throw "cloudflared not found on PATH. Pass -Cloudflared <path>, or get it from https://github.com/cloudflare/cloudflared/releases"
    }
    $Cloudflared = $cmd.Source
}
if (-not (Test-Path $Cloudflared)) { throw "no such file: $Cloudflared" }

New-Item -ItemType Directory -Force -Path $dist | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $dist "licenses") | Out-Null

Copy-Item (Join-Path $root "target\release\fst.exe") (Join-Path $dist "fst.exe") -Force
Copy-Item $Cloudflared (Join-Path $dist "cloudflared.exe") -Force

# licenses\ is kept in the repo; only warn if someone has emptied it
$apache = Join-Path $dist "licenses\LICENSE-Apache-2.0.txt"
if (-not (Test-Path $apache)) {
    Write-Warning "licenses\LICENSE-Apache-2.0.txt is missing - fetch it from https://www.apache.org/licenses/LICENSE-2.0.txt before redistributing"
}

$cfVer = (& $Cloudflared --version) -join " "
Write-Host ""
Write-Host "dist ready: $dist" -ForegroundColor Green
Get-ChildItem $dist -Recurse -File |
    Select-Object @{n = "file"; e = { $_.FullName.Substring($dist.Length + 1) } },
                  @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } } |
    Format-Table -AutoSize
Write-Host "bundled $cfVer"
Write-Host "total  $([math]::Round(((Get-ChildItem $dist -Recurse -File | Measure-Object Length -Sum).Sum / 1MB), 1)) MB"
