# Download and verify the native Windows release. Run with PowerShell 5.1+.
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
if (-not $Version) {
    $release = Invoke-RestMethod 'https://api.github.com/repos/dieterpl/butai/releases/latest'
    $Version = $release.tag_name
}
$Version = $Version -replace '^v', ''
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw "Invalid version: $Version" }
$stage = "butai-$Version-x86_64-pc-windows-msvc"
$asset = "$stage.tar.gz"
$temp = Join-Path ([IO.Path]::GetTempPath()) ("butai-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $archive = Join-Path $temp $asset
    Invoke-WebRequest "$repo/releases/download/v$Version/$asset" -OutFile $archive -UseBasicParsing
    $sums = (Invoke-WebRequest "$repo/releases/download/v$Version/SHA256SUMS" -UseBasicParsing).Content
    $expected = $null
    foreach ($line in ($sums -split '\r?\n')) {
        if ($line -match '^([a-fA-F0-9]{64})\s+\*?(.+)$' -and $Matches[2] -eq $asset) {
            $expected = $Matches[1]
            break
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
