# Fetches the libraries pinned in windows-deps.env, checks their digests, and
# unpacks them.
#
#   pwsh packaging/windows-deps.ps1 -Downloads <dir>
#   pwsh packaging/windows-deps.ps1 -Downloads <dir> -Lib <dir> [-Ffmpeg <dir>]
#
# The first form only fetches and checks. A file already in -Downloads is
# checked, not fetched again, so a cache of that folder outlives the pinned
# assets being deleted upstream. -Lib receives libmpv-2.dll and the
# vulkan-1.dll it imports; -Ffmpeg receives ffmpeg.exe.

param(
    [Parameter(Mandatory = $true)]
    [string] $Downloads,
    [string] $Lib,
    [string] $Ffmpeg
)

$ErrorActionPreference = 'Stop'

$pins = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-deps.env')) {
    if ($line -match '^([A-Z0-9_]+)=(.+)$') {
        $pins[$Matches[1]] = $Matches[2]
    }
}

New-Item -ItemType Directory -Force -Path $Downloads | Out-Null

function Get-Pinned([string] $name) {
    $url = $pins["${name}_URL"]
    $sha256 = $pins["${name}_SHA256"]
    if (-not $url -or -not $sha256) {
        throw "windows-deps.env has no ${name}_URL and ${name}_SHA256"
    }
    $file = Join-Path $Downloads ([IO.Path]::GetFileName($url))
    if (-not (Test-Path -LiteralPath $file)) {
        curl.exe -fsSL --retry 3 -o $file $url
        if ($LASTEXITCODE -ne 0) {
            throw "could not download $url; if it was deleted upstream, bump its pin in packaging/windows-deps.env"
        }
    }
    $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash
    if ($got -ne $sha256) {
        throw "SHA256 mismatch for $url`n expected $sha256`n got      $got"
    }
    $file
}

function Expand-One([string] $archive, [string] $out, [string] $name) {
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    7z e -y -r -bso0 -bsp0 "-o$out" $archive $name
    if ($LASTEXITCODE -ne 0) { throw "7z could not extract $name from $archive" }
    if (-not (Test-Path -LiteralPath (Join-Path $out $name))) { throw "$name is not in $archive" }
}

# Named apart from the parameters: PowerShell's variable names ignore case,
# so $ffmpeg would be $Ffmpeg.
$libmpvArchive = Get-Pinned 'WIN_LIBMPV'
$vulkanArchive = Get-Pinned 'WIN_VULKAN'
$ffmpegArchive = Get-Pinned 'WIN_FFMPEG'

if ($Lib) {
    Expand-One $libmpvArchive $Lib 'libmpv-2.dll'
    Expand-One $vulkanArchive $Lib 'vulkan-1.dll'
}
if ($Ffmpeg) {
    Expand-One $ffmpegArchive $Ffmpeg 'ffmpeg.exe'
    # Read in full: cutting the output short could leave a nonzero exit code,
    # which pwsh makes the step's.
    $version = & (Join-Path $Ffmpeg 'ffmpeg.exe') -hide_banner -version
    if ($LASTEXITCODE -ne 0) { throw 'ffmpeg.exe does not run' }
    $version[0]
}
