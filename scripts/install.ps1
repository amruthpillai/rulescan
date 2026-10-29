# Install a checksummed RuleScan release for the current user. No service is installed.
[CmdletBinding()]
param(
    [string] $Version = $(if ($env:RULESCAN_VERSION) { $env:RULESCAN_VERSION } else { 'latest' }),
    [string] $InstallDir = $(if ($env:RULESCAN_INSTALL_DIR) { $env:RULESCAN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\RuleScan' }),
    [switch] $NoModifyPath
)

& {
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Use install.sh on macOS or Linux.' }
Add-Type -AssemblyName System.IO.Compression.FileSystem
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -ne 'X64') { throw "Unsupported native Windows architecture: $architecture. This release provides Windows x64 binaries." }
if (-not [IO.Path]::IsPathRooted($InstallDir)) { throw 'InstallDir must be an absolute path.' }
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$target = Join-Path $InstallDir 'rulescan.exe'
$noticesTarget = Join-Path $InstallDir 'rulescan-notices.txt'
function Assert-RegularDestination([string] $Destination) {
    if (Test-Path -LiteralPath $Destination) {
        $item = Get-Item -LiteralPath $Destination -Force
        if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "Refusing to replace a directory or reparse point: $Destination"
        }
    }
}
Assert-RegularDestination $target
Assert-RegularDestination $noticesTarget

# Windows PowerShell 5.1 otherwise inherits older process TLS defaults.
$previousTls = [Net.ServicePointManager]::SecurityProtocol
try {
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$repository = 'https://github.com/amruthpillai/rulescan'
if ($Version -eq 'latest') {
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/amruthpillai/rulescan/releases/latest' -Headers @{ 'User-Agent' = 'RuleScan-installer' }
    $Version = [string] $release.tag_name
}
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:[-.][0-9A-Za-z.-]+)?$') { throw 'Version must be a release tag such as v0.1.0.' }
$asset = "rulescan-$Version-x86_64-pc-windows-msvc.zip"
$base = "$repository/releases/download/$Version"
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('rulescan-install-' + [Guid]::NewGuid().ToString('N'))
$stagedBinary = $null
$stagedNotices = $null
[void] [IO.Directory]::CreateDirectory($temporary)
try {
    $archive = Join-Path $temporary 'archive.zip'
    $checksumFile = Join-Path $temporary 'SHA256SUMS'
    Write-Host "Downloading RuleScan $Version (Windows x64)..."
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $checksumFile
    if ((Get-Item -LiteralPath $archive).Length -gt 134217728 -or (Get-Item -LiteralPath $checksumFile).Length -gt 1048576) { throw 'Release download exceeds its size budget.' }
    $matching = @(Get-Content -LiteralPath $checksumFile | Where-Object {
        $fields = $_ -split '\s+', 2
        $fields.Count -eq 2 -and $fields[1] -ceq $asset
    })
    if ($matching.Count -ne 1) { throw 'Release checksum is missing, duplicated, or invalid.' }
    if ($matching[0] -cnotmatch ('^[0-9a-f]{64}  ' + [Regex]::Escape($asset) + '$')) { throw 'Release checksum is invalid.' }
    $expected = $matching[0].Substring(0, 64)
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw 'SHA-256 mismatch; nothing was installed.' }

    $allowed = @('rulescan.exe', 'README.md', 'THIRD_PARTY_NOTICES.txt', 'manifest.json')
    $seen = @{}
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $size = [long] 0
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            $type = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if ($name -cnotin $allowed -or $seen.ContainsKey($name) -or ($type -ne 0 -and $type -ne 0x8000)) {
                throw 'Archive has an unexpected, duplicate, or non-regular entry.'
            }
            $seen[$name] = $true
            $size += $entry.Length
            if ($size -gt 134217728) { throw 'Archive exceeds the 128 MiB extraction budget.' }
        }
        if ($seen.Count -ne 4) { throw 'Archive is missing required files.' }
        $extracted = Join-Path $temporary 'extracted'
        [void] [IO.Directory]::CreateDirectory($extracted)
        foreach ($entry in $zip.Entries) {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $extracted $entry.FullName), $false)
        }
    } finally { $zip.Dispose() }
    $downloaded = Join-Path $extracted 'rulescan.exe'
    $reported = (& $downloaded --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $reported -cne ('rulescan ' + $Version.Substring(1))) {
        throw 'The downloaded executable does not report the requested version.'
    }
    [void] [IO.Directory]::CreateDirectory($InstallDir)
    $stagedBinary = Join-Path $InstallDir ('.rulescan-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    $stagedNotices = Join-Path $InstallDir ('.rulescan-notices-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    [IO.File]::Copy($downloaded, $stagedBinary, $false)
    [IO.File]::Copy((Join-Path $extracted 'THIRD_PARTY_NOTICES.txt'), $stagedNotices, $false)
    function Publish-File([string] $Staged, [string] $Destination) {
        Assert-RegularDestination $Destination
        # Windows PowerShell marshals $null to an empty string for .NET string parameters.
        if ([IO.File]::Exists($Destination)) { [IO.File]::Replace($Staged, $Destination, [NullString]::Value) }
        else { [IO.File]::Move($Staged, $Destination) }
    }
    Publish-File $stagedNotices $noticesTarget
    $stagedNotices = $null
    Publish-File $stagedBinary $target
    $stagedBinary = $null

    if (-not $NoModifyPath) {
        try {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $InstallDir) -join ';'), 'User')
        }
        if (-not ($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
            $env:Path = $env:Path.TrimEnd(';') + ';' + $InstallDir
        }
        } catch { Write-Warning "RuleScan is installed, but PATH could not be updated. Add $InstallDir to your user PATH." }
    }
    Write-Host "Installed RuleScan $Version to $target"
    Write-Host 'Run: rulescan'
} finally {
    if ($stagedBinary -and [IO.File]::Exists($stagedBinary)) { [IO.File]::Delete($stagedBinary) }
    if ($stagedNotices -and [IO.File]::Exists($stagedNotices)) { [IO.File]::Delete($stagedNotices) }
    if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
}
} finally { [Net.ServicePointManager]::SecurityProtocol = $previousTls }
}
