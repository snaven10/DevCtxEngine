# Install devctx from the latest GitHub release, on Windows.
#
#   irm https://raw.githubusercontent.com/snaven10/DevCtxEngine/main/install.ps1 | iex
#
# The counterpart of install.sh, and deliberately the same shape: it downloads
# one binary and puts it on your PATH. It writes no configuration, creates no
# project and fetches no model — those are choices, and choosing them for you is
# how a setup ends up in a state nobody can explain. AGENTS.md walks through
# them, and is written so a coding agent can follow it as well as a person.
#
# A .zip rather than the .tar.gz the other platforms get: Windows has had
# bsdtar since build 17063, but not reliably with gzip support in every shell a
# person might run this from, and Expand-Archive is always there.

$ErrorActionPreference = 'Stop'

$repo   = if ($env:DEVCTX_REPO)    { $env:DEVCTX_REPO }    else { 'snaven10/DevCtxEngine' }
$binDir = if ($env:DEVCTX_BIN_DIR) { $env:DEVCTX_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'devctx\bin' }

function Die($msg) { Write-Error "install.ps1: $msg"; exit 1 }

# Only x64 is published. ARM64 Windows can run the x64 build through emulation,
# but slowly enough that saying so beats installing it silently.
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
    Die "no prebuilt binary for $arch — build from source: cargo build --release"
}
$target = 'x86_64-pc-windows-msvc'

$tag = $env:DEVCTX_VERSION
if (-not $tag) {
    try {
        $latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" `
                                    -Headers @{ 'User-Agent' = 'devctx-install' }
        $tag = $latest.tag_name
    } catch {
        Die "could not determine the latest release of ${repo}: $_"
    }
}
if (-not $tag) { Die "could not determine the latest release of $repo" }

$name = "devctx-$target"
$url  = "https://github.com/$repo/releases/download/$tag/$name.zip"
$tmp  = Join-Path ([System.IO.Path]::GetTempPath()) ("devctx-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

try {
    Write-Host "Downloading devctx $tag ($target)…"
    $zip = Join-Path $tmp "$name.zip"
    # The progress bar makes Invoke-WebRequest an order of magnitude slower on a
    # binary this size, and nobody reads it inside a piped installer.
    $prev = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip
    } catch {
        Die "download failed: $url"
    } finally {
        $ProgressPreference = $prev
    }

    # Verify when the checksum is published; a corrupt download that runs is
    # worse than one that fails.
    try {
        $sumFile = Join-Path $tmp "$name.zip.sha256"
        Invoke-WebRequest -Uri "$url.sha256" -OutFile $sumFile -ErrorAction Stop
        # The file is `<hash>  <name>`, as shasum writes it.
        $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0]
        $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash
        if ($actual -ne $expected) { Die 'checksum mismatch' }
    } catch [System.Net.WebException] {
        # No checksum published for this release; carry on rather than refuse.
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    $exe = Join-Path $tmp "$name\devctx.exe"
    if (-not (Test-Path $exe)) { Die "the archive did not contain devctx.exe" }
    Copy-Item -Path $exe -Destination (Join-Path $binDir 'devctx.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

Write-Host "Installed $binDir\devctx.exe"

# Persist the PATH entry for the user, not just this shell: an installer whose
# effect vanishes when the window closes reads as one that failed.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$binDir", 'User')
    $env:Path = "$env:Path;$binDir"
    Write-Host "Added $binDir to your PATH — open a new terminal for it to take effect elsewhere."
}

Write-Host @"

Next: devctx needs an embedding model chosen before anything is indexed, and
changing it later means re-indexing from scratch.

    https://github.com/$repo/blob/main/AGENTS.md

That file is the setup procedure — including migrating memories from an older
DevAI install — written so a coding agent can carry it out. Point yours at it.
"@
