# Download and verify the native Windows release. Run with PowerShell 5.1+.
#
# Each file comes from GitHub's link, then the mirror on GitHub Pages, then the
# API's link, each tried twice: in November 2026 GitHub's anonymous downloads
# answered most requests with a 504 for days. See scripts/install.sh.
[CmdletBinding()]
param(
    [string]$Version = $env:BUTAI_VERSION,
    [string]$InstallDir = $env:BUTAI_INSTALL_DIR
)
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
if (-not [Environment]::Is64BitOperatingSystem) { throw 'butai requires 64-bit Windows.' }
if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\butai' }
$repo = 'https://github.com/dieterpl/butai'
$api = 'https://api.github.com/repos/dieterpl/butai'
$mirror = 'https://dieterpl.github.io/butai'

function Get-Answer([string]$Url) {
    for ($try = 1; $try -le 2; $try++) {
        try { return Invoke-RestMethod $Url -UseBasicParsing } catch { Start-Sleep -Seconds $try }
    }
    return $null
}

if (-not $Version) {
    $latest = Get-Answer "$api/releases/latest"
    if ($latest) {
        $Version = $latest.tag_name
    } else {
        $Version = (Get-Answer "$mirror/latest")
        if ($Version) { $Version = $Version.Trim() }
    }
    if (-not $Version) { throw 'Could not find the latest release on GitHub or its mirror; set BUTAI_VERSION.' }
}
$Version = $Version -replace '^v', ''
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw "Invalid version: $Version" }
$stage = "butai-$Version-x86_64-pc-windows-msvc"
$asset = "$stage.tar.gz"

# GitHub's description of the release: each file's API link and the sha256 it
# recorded at upload. Missing when the API is down; the mirror and SHA256SUMS
# cover that.
$release = Get-Answer "$api/releases/tags/v$Version"

function Save-Asset([string]$Name, [string]$OutFile) {
    $urls = @("$repo/releases/download/v$Version/$Name", "$mirror/v$Version/$Name")
    $known = $null
    if ($release) { $known = $release.assets | Where-Object { $_.name -eq $Name } | Select-Object -First 1 }
    if ($known -and $known.url) { $urls += $known.url }
    foreach ($url in $urls) {
        for ($try = 1; $try -le 2; $try++) {
            try {
                Invoke-WebRequest $url -OutFile $OutFile -UseBasicParsing -Headers @{ Accept = 'application/octet-stream' }
                return
            } catch {
                # A missing file is not worth a second try.
                $code = $null
                if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
                if ($code -eq 404 -or $code -eq 403) { break }
                Start-Sleep -Seconds $try
            }
        }
    }
    throw "Could not download $Name from GitHub, its mirror or its API."
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ("butai-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $archive = Join-Path $temp $asset
    Save-Asset $asset $archive

    # Against GitHub's record when the API answered — that holds wherever the
    # bytes came from — else against the release's SHA256SUMS.
    $expected = $null
    if ($release) {
        $digest = ($release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1).digest
        if ($digest -match '^sha256:([a-fA-F0-9]{64})$') { $expected = $Matches[1] }
    }
    if (-not $expected) {
        $sumsFile = Join-Path $temp 'SHA256SUMS'
        Save-Asset 'SHA256SUMS' $sumsFile
        foreach ($line in (Get-Content $sumsFile)) {
            if ($line -match '^([a-fA-F0-9]{64})\s+\*?(.+)$' -and $Matches[2] -eq $asset) {
                $expected = $Matches[1]
                break
            }
        }
    }
    if (-not $expected) { throw "No checksum for $asset" }
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $expected) { throw 'Release checksum verification failed.' }
    # Extract only the executable from the verified archive.
    & tar.exe -xzf $archive -C $temp "$stage/butai.exe"
    if ($LASTEXITCODE -ne 0) { throw 'Could not unpack release (tar.exe is required).' }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item (Join-Path $temp "$stage\butai.exe") (Join-Path $InstallDir 'butai.exe') -Force
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    if (($userPath -split ';') -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable('Path', (($userPath.TrimEnd(';') + ';' + $InstallDir).TrimStart(';')), 'User')
    }
    if (($env:Path -split ';') -notcontains $InstallDir) { $env:Path += ";$InstallDir" }
    Write-Host "Installed butai $Version to $InstallDir. Run: butai"
} finally {
    Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
}
