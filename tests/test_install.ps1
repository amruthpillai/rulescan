# Native Windows installer smoke. Downloads are served from the actual local package.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Archive,
    [string] $Version = '0.1.0'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
Add-Type -AssemblyName System.IO.Compression.FileSystem
$script:FixtureTag = 'v' + $Version.TrimStart('v')
$script:FixtureAsset = "rulescan-$script:FixtureTag-x86_64-pc-windows-msvc.zip"
if ([IO.Path]::GetFileName($Archive) -cne $script:FixtureAsset) { throw 'Pass the native Windows x64 archive.' }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('rulescan-installer-test-' + [Guid]::NewGuid().ToString('N'))
$script:FixtureRelease = Join-Path $temporary 'release'
[void] [IO.Directory]::CreateDirectory($script:FixtureRelease)
[IO.File]::Copy([IO.Path]::GetFullPath($Archive), (Join-Path $script:FixtureRelease $script:FixtureAsset))
$script:FixtureHash = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
$script:FixtureChecksum = "$script:FixtureHash  $script:FixtureAsset`n"
$script:RequestedUrls = @()
$installer = Join-Path (Split-Path $PSScriptRoot -Parent) 'scripts\install.ps1'
$initialPath = $env:Path

function Invoke-RestMethod {
    param([string] $Uri, [hashtable] $Headers)
    if ($Uri -cne 'https://api.github.com/repos/amruthpillai/rulescan/releases/latest') { throw "Unexpected metadata URL: $Uri" }
    $script:RequestedUrls += $Uri
    return @{ tag_name = $script:FixtureTag }
}
function Invoke-WebRequest {
    param([string] $Uri, [string] $OutFile, [switch] $UseBasicParsing)
    $script:RequestedUrls += $Uri
    $prefix = "https://github.com/amruthpillai/rulescan/releases/download/$script:FixtureTag/"
    if ($Uri -ceq ($prefix + 'SHA256SUMS')) {
        [IO.File]::WriteAllText($OutFile, $script:FixtureChecksum, [Text.UTF8Encoding]::new($false))
    } elseif ($Uri -ceq ($prefix + $script:FixtureAsset)) {
        [IO.File]::Copy((Join-Path $script:FixtureRelease $script:FixtureAsset), $OutFile)
    } else { throw "Unexpected asset URL: $Uri" }
}
function Expect-Failure([ScriptBlock] $Action, [string] $Message) {
    $failed = $false
    try { & $Action } catch {
        $failed = $true
        if ($_.Exception.Message -notlike "*$Message*") { throw }
    }
    if (-not $failed) { throw "Expected installer failure: $Message" }
}

try {
    $destination = Join-Path $temporary 'destination with spaces'
    & $installer -InstallDir $destination -NoModifyPath
    $installed = Join-Path $destination 'rulescan.exe'
    if ((& $installed --version | Out-String).Trim() -cne ('rulescan ' + $script:FixtureTag.Substring(1))) { throw 'Installed version mismatch.' }
    if (-not [IO.File]::Exists((Join-Path $destination 'rulescan-notices.txt'))) { throw 'Third-party notices were not installed.' }
    if ($script:RequestedUrls.Count -ne 3) { throw 'Latest metadata must be fetched once, then tag-specific assets.' }
    if ($env:Path -cne $initialPath) { throw 'NoModifyPath changed the process PATH.' }
    $originalHash = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash

    $script:FixtureChecksum = ('0' * 64) + "  $script:FixtureAsset`n"
    Expect-Failure { & $installer -Version $script:FixtureTag -InstallDir $destination -NoModifyPath } 'SHA-256 mismatch'
    if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne $originalHash) { throw 'Checksum failure replaced the existing binary.' }
    $script:FixtureChecksum = "$script:FixtureHash  $script:FixtureAsset`ninvalid  $script:FixtureAsset`n"
    Expect-Failure { & $installer -Version $script:FixtureTag -InstallDir $destination -NoModifyPath } 'checksum is missing, duplicated, or invalid'

    $script:FixtureChecksum = "$script:FixtureHash  $script:FixtureAsset`n"
    & $installer -Version $script:FixtureTag -InstallDir $destination -NoModifyPath
    $blocked = Join-Path $temporary 'blocked'
    [void] [IO.Directory]::CreateDirectory((Join-Path $blocked 'rulescan.exe'))
    Expect-Failure { & $installer -Version $script:FixtureTag -InstallDir $blocked -NoModifyPath } 'Refusing to replace'

    $malicious = Join-Path $script:FixtureRelease $script:FixtureAsset
    [IO.File]::Delete($malicious)
    $zip = [IO.Compression.ZipFile]::Open($malicious, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $zip.CreateEntry('../escape.txt')
        $writer = [IO.StreamWriter]::new($entry.Open())
        try { $writer.Write('Must not escape extraction.') } finally { $writer.Dispose() }
    } finally { $zip.Dispose() }
    $script:FixtureHash = (Get-FileHash -LiteralPath $malicious -Algorithm SHA256).Hash.ToLowerInvariant()
    $script:FixtureChecksum = "$script:FixtureHash  $script:FixtureAsset`n"
    Expect-Failure { & $installer -Version $script:FixtureTag -InstallDir $destination -NoModifyPath } 'Archive has an unexpected'
    if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne $originalHash) { throw 'Invalid archive replaced the binary.' }
    if ([IO.File]::Exists((Join-Path $temporary 'escape.txt'))) { throw 'Archive escaped extraction.' }
    Write-Host 'Windows installer: native install, upgrade, checksum/duplicate rejection, directory refusal and traversal protection passed.'
} finally {
    if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
}
