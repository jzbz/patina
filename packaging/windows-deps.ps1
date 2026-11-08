# Fetches the libraries pinned in windows-deps.env, and the yt-dlp pinned in
# bundled-tools.env, checks their digests, and unpacks them.
#
#   pwsh packaging/windows-deps.ps1 -Downloads <dir>
#   pwsh packaging/windows-deps.ps1 -Downloads <dir> -Lib <dir> [-Ffmpeg <dir>] [-Tools <dir>]
#
# The first form only fetches and checks the libraries. A file already in
# -Downloads is checked, not fetched again, so a cache of that folder
# outlives the pinned assets being deleted upstream. -Lib receives
# libmpv-2.dll, its licence as LICENSE-libmpv.txt and the LGPLv3 its FFmpeg
# is under as LICENSE-LGPLv3.txt; -Ffmpeg receives the tests' ffmpeg.exe, a
# full build. -Tools receives the yt-dlp the Patina folder
# carries for online video: yt-dlp_win.zip unpacked as it is into a yt-dlp
# folder, yt-dlp.exe and the _internal folder it loads Python and its
# libraries from, where Patina looks for it (src/platform.rs, `bundled`);
# with the notices from that _internal as THIRD-PARTY-yt-dlp.txt, and
# LICENSE-GPLv3.txt, its licence, beside the folder. Those are fetched only
# when it is given. The folder's own ffmpeg.exe is not fetched but built, by
# packaging/ffmpeg.sh in release.yml's ffmpeg-windows job.

param(
    [Parameter(Mandatory = $true)]
    [string] $Downloads,
    [string] $Lib,
    [string] $Ffmpeg,
    [string] $Tools
)

$ErrorActionPreference = 'Stop'

$pins = @{}
foreach ($file in 'windows-deps.env', 'bundled-tools.env') {
    foreach ($line in Get-Content -LiteralPath (Join-Path $PSScriptRoot $file)) {
        if ($line -match '^([A-Z0-9_]+)=(.+)$') {
            $pins[$Matches[1]] = $Matches[2]
        }
    }
}

New-Item -ItemType Directory -Force -Path $Downloads | Out-Null

function Get-Pinned([string] $name) {
    $url = $pins["${name}_URL"]
    $sha256 = $pins["${name}_SHA256"]
    if (-not $url -or -not $sha256) {
        throw "windows-deps.env and bundled-tools.env have no ${name}_URL and ${name}_SHA256"
    }
    $file = Join-Path $Downloads ([IO.Path]::GetFileName($url))
    if (-not (Test-Path -LiteralPath $file)) {
        curl.exe -fsSL --retry 3 -o $file $url
        if ($LASTEXITCODE -ne 0) {
            throw "could not download $url; if it was deleted upstream, bump its pin in packaging/windows-deps.env or bundled-tools.env"
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
$ffmpegArchive = Get-Pinned 'WIN_FFMPEG'

if ($Lib) {
    Expand-One $libmpvArchive $Lib 'libmpv-2.dll'
    Expand-One $libmpvArchive $Lib 'LICENSE.LGPL'
    Move-Item -Force -LiteralPath (Join-Path $Lib 'LICENSE.LGPL') -Destination (Join-Path $Lib 'LICENSE-libmpv.txt')
    Copy-Item -LiteralPath (Get-Pinned 'WIN_LGPLV3_LICENSE') -Destination (Join-Path $Lib 'LICENSE-LGPLv3.txt')
}
if ($Tools) {
    $ytdlp = Get-Pinned 'YTDLP_WINDOWS'
    $gplv3 = Get-Pinned 'GPLV3_LICENSE'
    # Into a folder that holds nothing yet, so that what it holds afterwards
    # is the zip and nothing else; the zip's digest is the pin on all of it.
    $folder = Join-Path $Tools 'yt-dlp'
    if (Test-Path -LiteralPath $folder) { throw "$folder is there already; -Tools unpacks yt-dlp into a folder of its own" }
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    7z x -y -bso0 -bsp0 "-o$folder" $ytdlp
    if ($LASTEXITCODE -ne 0) { throw "7z could not extract $ytdlp" }
    $found = @(Get-ChildItem -LiteralPath $folder -Force | ForEach-Object Name)
    if (Compare-Object @('yt-dlp.exe', '_internal') $found) {
        throw "$ytdlp holds $($found -join ', '), not yt-dlp.exe and _internal; see packaging/bundled-tools.env"
    }
    Copy-Item -LiteralPath (Join-Path $folder '_internal\THIRD_PARTY_LICENSES.txt') -Destination (Join-Path $Tools 'THIRD-PARTY-yt-dlp.txt')
    Copy-Item -LiteralPath $gplv3 -Destination (Join-Path $Tools 'LICENSE-GPLv3.txt')
}
if ($Ffmpeg) {
    Expand-One $ffmpegArchive $Ffmpeg 'ffmpeg.exe'
    # Read in full: cutting the output short could leave a nonzero exit code,
    # which pwsh makes the step's.
    $version = & (Join-Path $Ffmpeg 'ffmpeg.exe') -hide_banner -version
    if ($LASTEXITCODE -ne 0) { throw 'ffmpeg.exe does not run' }
    $version[0]
}
