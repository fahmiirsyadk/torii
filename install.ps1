$ErrorActionPreference = "Stop"

$repo = "fahmiirsyadk/torii"
$installRoot = if ($env:TORII_HOME) { $env:TORII_HOME } else {
    Join-Path $env:LOCALAPPDATA "Torii"
}
$binDir = if ($env:TORII_BIN_DIR) { $env:TORII_BIN_DIR } else {
    Join-Path $installRoot "bin"
}
$architecture = [string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    default { throw "Torii does not publish a Windows build for $architecture" }
}

$release = Invoke-RestMethod `
    -Headers @{ "User-Agent" = "torii-installer" } `
    -Uri "https://api.github.com/repos/$repo/releases/latest"
$tag = [string]$release.tag_name
if (-not $tag) { throw "GitHub's latest release response has no tag name" }
$version = $tag -replace '^v', ''
if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$') {
    throw "Invalid Torii version: $version"
}
$assetName = "torii-v$version-$target.zip"
$asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
if (-not $asset) { throw "Release v$version has no asset for $target" }
$digest = [string]$asset.digest
if ($digest -notmatch '^sha256:[0-9a-fA-F]{64}$') {
    throw "Release asset $assetName has no SHA-256 digest"
}
$downloadUrl = [string]$asset.browser_download_url
if ($downloadUrl -notmatch '^https://github\.com/') {
    throw "Release asset $assetName has no trusted download URL"
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) "torii-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $archive = Join-Path $temporary $assetName
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archive
    $expected = ($digest -replace '^sha256:', '').ToLowerInvariant()
    $actual = [string](Get-FileHash -Algorithm SHA256 $archive).Hash
    if (-not $actual) { throw "Could not calculate the SHA-256 digest for $assetName" }
    if ($actual -ne $expected) { throw "Checksum verification failed for $assetName" }

    $versionDir = Join-Path $installRoot "versions\$version"
    New-Item -ItemType Directory -Force -Path $versionDir, $binDir | Out-Null
    Expand-Archive -Path $archive -DestinationPath $versionDir -Force
    $versionExe = Join-Path $versionDir "bin\torii.exe"
    $sidecarExe = Join-Path $versionDir "libexec\torii-sidecar.exe"
    if (-not (Test-Path $versionExe) -or -not (Test-Path $sidecarExe)) {
        throw "Release archive is incomplete"
    }

    Copy-Item $versionExe (Join-Path $binDir "torii.exe") -Force
    $current = Join-Path $installRoot "current"
    if (Test-Path $current) {
        Copy-Item $current (Join-Path $installRoot "previous") -Force
    }
    $pointer = Join-Path $installRoot ".current.new"
    [System.IO.File]::WriteAllText($pointer, "$version`n")
    Move-Item $pointer $current -Force
    $pending = Join-Path $installRoot ".pending.new"
    [System.IO.File]::WriteAllText($pending, "$version`n")
    Move-Item $pending (Join-Path $installRoot "pending") -Force

    $userPath = [string][Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @($userPath -split ";" | Where-Object { $_ })
    if ($parts -notcontains $binDir) {
        [Environment]::SetEnvironmentVariable("Path", (($parts + $binDir) -join ";"), "User")
        $env:Path = "$env:Path;$binDir"
    }
    Write-Host "Installed Torii v$version at $installRoot"
} finally {
    Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}
