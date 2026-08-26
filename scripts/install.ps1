<#
.SYNOPSIS
    Installs the Mast CLI (mast.exe and mast-daemon.exe) on Windows.

.DESCRIPTION
    The Windows counterpart to scripts/install.sh, which cannot help here: the
    `curl | sh` form needs a POSIX shell, and Windows has none by default.

        irm https://mast.sh/install.ps1 | iex

    Nothing is installed system-wide and no elevation is requested — the
    binaries land under %LOCALAPPDATA% and only the user's own PATH is touched.

    By default this matches the Mast desktop app already installed on this
    machine rather than taking the newest release. The CLI and the app share a
    per-user socket and refuse to run as a mismatched pair, so "latest" is the
    wrong default whenever an app is already sitting there.

    Arguments cannot be passed through `iex`, so pass them by invoking the
    downloaded text as a script block instead:

        & ([scriptblock]::Create((irm https://mast.sh/install.ps1))) -Version v0.5.0

.PARAMETER Version
    Release tag to install (e.g. v0.5.0). Overrides the desktop match.

.PARAMETER Dir
    Install directory. Defaults to %LOCALAPPDATA%\Programs\Mast\bin.
#>
[CmdletBinding()]
param(
    [string] $Version = $env:MAST_VERSION,
    [string] $Dir     = $env:MAST_INSTALL_DIR
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Repo     = 'stackrats/mast'
$Releases = "https://github.com/$Repo/releases"

# Windows PowerShell 5.1 still negotiates TLS 1.0 by default on older builds,
# which github.com refuses outright. Opt in before the first request.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # PowerShell 7 on modern Windows manages this itself; nothing to do.
}

function Write-Step { param([string] $Message) Write-Host "==> $Message" -ForegroundColor Blue }
function Write-Note { param([string] $Message) Write-Host "    $Message" -ForegroundColor DarkGray }
function Write-Warn { param([string] $Message) Write-Host " warn $Message" -ForegroundColor Yellow }
function Fail      { param([string] $Message) Write-Host "error $Message" -ForegroundColor Red; exit 1 }

# The compatibility unit for the daemon socket, mirroring
# mast_contract::wire_compat_key: major.minor. Patch releases never move a wire
# shape; a minor bump is where the DTOs are allowed to grow.
function Get-MinorKey {
    param([string] $Value)
    if ([string]::IsNullOrWhiteSpace($Value)) { return '' }
    $parts = $Value.TrimStart('v').Split('.')
    if ($parts.Count -ge 2) { return "$($parts[0]).$($parts[1])" }
    return $parts[0]
}

# ---------------------------------------------------------------- platform ---

function Get-Platform {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        'AMD64' { return 'windows-x86_64' }
        'ARM64' {
            # No native arm64 Windows build yet. Windows 11 on ARM runs x64
            # binaries under emulation, so install those and say so rather than
            # refusing a machine that will work fine.
            Write-Warn 'No native ARM64 build yet — installing the x64 binaries, which Windows runs under emulation.'
            return 'windows-x86_64'
        }
        'x86' {
            Fail '32-bit Windows is not supported. Mast needs a 64-bit system.'
        }
        default {
            Fail "unrecognised processor architecture '$arch'. Open an issue at https://github.com/$Repo/issues."
        }
    }
}

# ----------------------------------------------------------- desktop probe ---

# The version of the Mast desktop app already installed, or $null. The NSIS
# bundle writes a standard uninstall entry; reading the registry is a query, not
# an exec, so this never launches the app to ask it.
function Get-DesktopVersion {
    $roots = @(
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    foreach ($root in $roots) {
        try {
            $entry = Get-ItemProperty -Path $root -ErrorAction SilentlyContinue |
                Where-Object { $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -eq 'Mast' } |
                Select-Object -First 1
            if ($entry -and $entry.PSObject.Properties['DisplayVersion'] -and $entry.DisplayVersion) {
                return [pscustomobject]@{ Version = $entry.DisplayVersion; Source = 'the installed Mast app' }
            }
        } catch {
            # An unreadable hive is not a reason to abandon the install.
        }
    }
    return $null
}

# ----------------------------------------------------------------- version ---

# /releases/latest redirects to /releases/tag/vX.Y.Z. Reading that redirect
# costs one request and, unlike the JSON API's 60-per-hour anonymous cap, will
# not strand a whole office behind one NAT gateway.
function Get-LatestTag {
    try {
        $response = Invoke-WebRequest -Uri "$Releases/latest" -MaximumRedirection 0 `
            -UseBasicParsing -ErrorAction SilentlyContinue
        $location = $response.Headers['Location']
    } catch {
        # Windows PowerShell throws on an unfollowed redirect; the response it
        # carries still has the header we came for.
        $location = $null
        if ($_.Exception.PSObject.Properties['Response'] -and $_.Exception.Response) {
            try { $location = $_.Exception.Response.Headers['Location'] } catch { $location = $null }
        }
    }
    if ($location) { return ([string]$location).Split('/')[-1] }

    # Fall back to the API rather than give up; it is rate-limited, not broken.
    try {
        return (Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -UseBasicParsing -Headers @{ 'User-Agent' = 'mast-installer' }).tag_name
    } catch {
        Fail "could not work out the latest release tag. Check $Releases and retry with -Version v<x.y.z>."
    }
}

# -------------------------------------------------------------------- main ---

$platform = Get-Platform
$desktop  = Get-DesktopVersion
$matchedDesktop = $false

if ($Version) {
    $tag = $Version
    if ($desktop -and (Get-MinorKey $tag) -ne (Get-MinorKey $desktop.Version)) {
        Write-Warn "installing CLI $($tag.TrimStart('v')) alongside desktop $($desktop.Version)."
        Write-Note 'Those two cannot share the daemon socket. Drop -Version to match the app instead.'
    }
} elseif ($desktop) {
    $tag = "v$($desktop.Version)"
    $matchedDesktop = $true
    Write-Step "Matching the Mast desktop app already installed ($($desktop.Version))"
    Write-Note "from $($desktop.Source) — pass -Version to override"
} else {
    $tag = Get-LatestTag
}

if ($tag -notmatch '^v') { $tag = "v$tag" }
$version = $tag.TrimStart('v')

if (-not $Dir) { $Dir = Join-Path $env:LOCALAPPDATA 'Programs\Mast\bin' }

$archive = "mast-$version-$platform.zip"
$url     = "$Releases/download/$tag/$archive"
$work    = Join-Path ([System.IO.Path]::GetTempPath()) ("mast-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work -Force | Out-Null

try {
    Write-Step "Downloading Mast $version for $platform"
    Write-Note $url

    $zip = Join-Path $work $archive
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        if ($matchedDesktop) {
            # A desktop-matched tag can be one no release carries a CLI for.
            # Falling back beats stopping, but it is a change of plan and gets
            # said out loud.
            Write-Warn "no $platform CLI archive published for $tag (matched from the desktop app)."
            $tag = Get-LatestTag
            if ($tag -notmatch '^v') { $tag = "v$tag" }
            $version = $tag.TrimStart('v')
            $archive = "mast-$version-$platform.zip"
            $url     = "$Releases/download/$tag/$archive"
            $zip     = Join-Path $work $archive
            $matchedDesktop = $false
            Write-Note "falling back to the latest release, $version"
            if ($desktop -and (Get-MinorKey $version) -ne (Get-MinorKey $desktop.Version)) {
                Write-Warn "this leaves CLI $version next to desktop $($desktop.Version) — they will not share a socket."
            }
            Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
        } else {
            Fail "download failed: $url`n      $($_.Exception.Message)`n      If $tag is real, it may not carry a $platform CLI archive — check $Releases/tag/$tag."
        }
    }

    # Releases do not all carry a SHA256SUMS asset. Verify when one is
    # published, say so plainly when there is nothing to verify against, and
    # never pass silently on a file that disagrees.
    $sums = Join-Path $work 'SHA256SUMS'
    $expected = $null
    try {
        Invoke-WebRequest -Uri "$Releases/download/$tag/SHA256SUMS" -OutFile $sums -UseBasicParsing
        foreach ($line in Get-Content $sums) {
            $fields = $line -split '\s+'
            if ($fields.Count -ge 2 -and $fields[1].TrimStart('*') -eq $archive) {
                $expected = $fields[0]
                break
            }
        }
    } catch {
        $expected = $null
    }
    if ($expected) {
        $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected.ToLowerInvariant()) {
            Fail "checksum mismatch for ${archive}.`n      expected $expected`n      got      $actual`n      Refusing to install."
        }
        Write-Note 'sha256 verified'
    } else {
        Write-Note "no SHA256SUMS published for $tag — skipping checksum verification"
    }

    $unpacked = Join-Path $work 'unpacked'
    Expand-Archive -Path $zip -DestinationPath $unpacked -Force
    foreach ($name in 'mast.exe', 'mast-daemon.exe') {
        if (-not (Test-Path (Join-Path $unpacked $name))) { Fail "$name missing from $archive." }
    }

    Write-Step "Installing to $Dir"
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    foreach ($name in 'mast.exe', 'mast-daemon.exe') {
        $target = Join-Path $Dir $name
        try {
            Copy-Item -Path (Join-Path $unpacked $name) -Destination $target -Force
        } catch {
            # Windows locks a running image, unlike the rename-over trick the
            # POSIX installer uses. Name the cause instead of the raw error.
            Fail "could not replace $target.`n      Close the Mast app and any running '$name', then run this again."
        }
        Write-Host "    OK $target" -ForegroundColor Green
    }
} finally {
    Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host "Mast $version installed." -ForegroundColor Green
if ($matchedDesktop) { Write-Note 'Matched to the desktop app, so both ends share one engine.' }

# Only the user's own PATH is touched, so this needs no elevation and cannot
# affect anyone else on the machine.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$onPath = $userPath.Split(';') | Where-Object { $_ -and ($_.TrimEnd('\') -ieq $Dir.TrimEnd('\')) }
if (-not $onPath) {
    $updated = if ($userPath.TrimEnd(';')) { "$($userPath.TrimEnd(';'));$Dir" } else { $Dir }
    [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    $env:Path = "$env:Path;$Dir"
    Write-Host ''
    Write-Note "Added $Dir to your PATH. Open a new terminal for it to take effect."
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Write-Host ''
    Write-Warn 'Docker was not found on PATH. Mast needs Docker Desktop with the WSL2 backend'
    Write-Note 'before `mast status` can do anything.'
}

Write-Host ''
Write-Note "The desktop app is a separate download: $Releases/latest"
