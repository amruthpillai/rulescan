# Native Windows installer smoke. Downloads are served from the actual local package.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Archive,
    [string] $Version = '0.1.0'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
Add-Type -AssemblyName System.IO.Compression.FileSystem
$fixture = [PSCustomObject]@{
    Tag = 'v' + $Version.TrimStart('v')
    Asset = "rulescan-v$($Version.TrimStart('v'))-x86_64-pc-windows-msvc.zip"
    Release = $null
    Hash = $null
    Checksum = $null
    RequestedUrls = [Collections.Generic.List[string]]::new()
    ModernResponse = $false
}
if ([IO.Path]::GetFileName($Archive) -cne $fixture.Asset) { throw 'Pass the native Windows x64 archive.' }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('rulescan-installer-test-' + [Guid]::NewGuid().ToString('N'))
$fixture.Release = Join-Path $temporary 'release'
[void] [IO.Directory]::CreateDirectory($fixture.Release)
[IO.File]::Copy([IO.Path]::GetFullPath($Archive), (Join-Path $fixture.Release $fixture.Asset))
$fixture.Hash = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
$fixture.Checksum = "$($fixture.Hash)  $($fixture.Asset)`n"
$installer = Join-Path (Split-Path $PSScriptRoot -Parent) 'scripts\install.ps1'
$initialPath = $env:Path

# Capture one mutable fixture object rather than resolving script: in the installer's scope.
Set-Item Function:Invoke-WebRequest -Value ({
    param([string] $Uri, [string] $OutFile, [switch] $UseBasicParsing)
    $fixture.RequestedUrls.Add($Uri)
    if ($Uri -ceq 'https://github.com/amruthpillai/rulescan/releases/latest') {
        $resolved = [Uri] ("https://github.com/amruthpillai/rulescan/releases/tag/" + $fixture.Tag)
        if ($fixture.ModernResponse) {
            return [PSCustomObject]@{ BaseResponse = [PSCustomObject]@{
                RequestMessage = [PSCustomObject]@{ RequestUri = $resolved }
            } }
        }
        return [PSCustomObject]@{ BaseResponse = [PSCustomObject]@{
            ResponseUri = $resolved
        } }
    }
    $prefix = "https://github.com/amruthpillai/rulescan/releases/download/$($fixture.Tag)/"
    if ($Uri -ceq ($prefix + 'SHA256SUMS')) {
        [IO.File]::WriteAllText($OutFile, $fixture.Checksum, [Text.UTF8Encoding]::new($false))
    } elseif ($Uri -ceq ($prefix + $fixture.Asset)) {
        [IO.File]::Copy((Join-Path $fixture.Release $fixture.Asset), $OutFile)
    } else { throw "Unexpected asset URL: $Uri" }
}.GetNewClosure())
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
    if ((& $installed --version | Out-String).Trim() -cne ('rulescan ' + $fixture.Tag.Substring(1))) { throw 'Installed version mismatch.' }
    if (-not [IO.File]::Exists((Join-Path $destination 'rulescan-notices.txt'))) { throw 'Third-party notices were not installed.' }
    if ($fixture.RequestedUrls.Count -ne 3) { throw 'Latest metadata must be fetched once, then tag-specific assets.' }
    if ($fixture.RequestedUrls[0] -cne 'https://github.com/amruthpillai/rulescan/releases/latest') { throw 'Latest resolution must use the public redirect, not the REST API.' }
    if ($env:Path -cne $initialPath) { throw 'NoModifyPath changed the process PATH.' }
    $originalHash = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash
    $fixture.ModernResponse = $true
    & $installer -InstallDir $destination -NoModifyPath
    if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne $originalHash) { throw 'Modern response resolution changed the executable.' }

    $fixture.Checksum = ('0' * 64) + "  $($fixture.Asset)`n"
    Expect-Failure { & $installer -Version $fixture.Tag -InstallDir $destination -NoModifyPath } 'SHA-256 mismatch'
    if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne $originalHash) { throw 'Checksum failure replaced the existing binary.' }
    $fixture.Checksum = "$($fixture.Hash)  $($fixture.Asset)`ninvalid  $($fixture.Asset)`n"
    Expect-Failure { & $installer -Version $fixture.Tag -InstallDir $destination -NoModifyPath } 'checksum is missing, duplicated, or invalid'

    $fixture.Checksum = "$($fixture.Hash)  $($fixture.Asset)`n"
    & $installer -Version $fixture.Tag -InstallDir $destination -NoModifyPath
    $blocked = Join-Path $temporary 'blocked'
    [void] [IO.Directory]::CreateDirectory((Join-Path $blocked 'rulescan.exe'))
    Expect-Failure { & $installer -Version $fixture.Tag -InstallDir $blocked -NoModifyPath } 'Refusing to replace'

    $malicious = Join-Path $fixture.Release $fixture.Asset
    [IO.File]::Delete($malicious)
    $zip = [IO.Compression.ZipFile]::Open($malicious, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $zip.CreateEntry('../escape.txt')
        $writer = [IO.StreamWriter]::new($entry.Open())
        try { $writer.Write('Must not escape extraction.') } finally { $writer.Dispose() }
    } finally { $zip.Dispose() }
    $fixture.Hash = (Get-FileHash -LiteralPath $malicious -Algorithm SHA256).Hash.ToLowerInvariant()
    $fixture.Checksum = "$($fixture.Hash)  $($fixture.Asset)`n"
    Expect-Failure { & $installer -Version $fixture.Tag -InstallDir $destination -NoModifyPath } 'Archive has an unexpected'
    if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -cne $originalHash) { throw 'Invalid archive replaced the binary.' }
    if ([IO.File]::Exists((Join-Path $temporary 'escape.txt'))) { throw 'Archive escaped extraction.' }
    Write-Host 'Windows installer: native install, upgrade, checksum/duplicate rejection, directory refusal and traversal protection passed.'
} finally {
    if ([IO.Directory]::Exists($temporary)) { [IO.Directory]::Delete($temporary, $true) }
}
