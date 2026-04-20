$ErrorActionPreference = "Stop"

$repo = if ($env:ASC_SYNC_INSTALL_REPO) {
    $env:ASC_SYNC_INSTALL_REPO
} else {
    "iivankin/asc-sync"
}

$installDir = if ($env:ASC_SYNC_INSTALL_DIR) {
    $env:ASC_SYNC_INSTALL_DIR
} else {
    Join-Path $HOME ".local\bin"
}

$arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    "X64" { "x86_64" }
    "Arm64" { "arm64" }
    default { throw "unsupported architecture: $($_.ToString())" }
}

$asset = "asc-sync-windows-$arch.zip"

switch ($asset) {
    "asc-sync-windows-x86_64.zip" { }
    default { throw "no published binary for windows-$arch" }
}

$url = "https://github.com/$repo/releases/latest/download/$asset"
$tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("asc-sync-install-" + [System.Guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $tmpDir $asset
$binaryPath = Join-Path $installDir "asc-sync.exe"

function Add-InstallDirToUserPath {
    param(
        [Parameter(Mandatory = $true)]
        [string] $PathToAdd
    )

    $currentUserPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
    $pathEntries = @()

    if ($currentUserPath) {
        $pathEntries = $currentUserPath.Split(';', [System.StringSplitOptions]::RemoveEmptyEntries)
    }

    $alreadyPresent = $pathEntries | Where-Object {
        $_.TrimEnd('\').ToLowerInvariant() -eq $PathToAdd.TrimEnd('\').ToLowerInvariant()
    }

    if ($alreadyPresent) {
        return $false
    }

    $updatedUserPath = if ($currentUserPath) {
        "$currentUserPath;$PathToAdd"
    } else {
        $PathToAdd
    }

    [Environment]::SetEnvironmentVariable("Path", $updatedUserPath, [EnvironmentVariableTarget]::User)
    return $true
}

try {
    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

    Write-Host "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $archivePath

    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Expand-Archive -Path $archivePath -DestinationPath $tmpDir -Force
    Copy-Item (Join-Path $tmpDir "asc-sync.exe") $binaryPath -Force

    $pathUpdated = Add-InstallDirToUserPath -PathToAdd $installDir

    Write-Host "installed asc-sync to $binaryPath"
    if ($pathUpdated) {
        Write-Host "added $installDir to your user PATH"
        Write-Host "open a new terminal window before running asc-sync"
    } else {
        Write-Host "$installDir is already in your user PATH"
    }
} finally {
    if (Test-Path $tmpDir) {
        Remove-Item -Path $tmpDir -Recurse -Force
    }
}
