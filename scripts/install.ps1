# Hooklistener CLI installer script for Windows
# Usage: irm https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.ps1 | iex

$ErrorActionPreference = 'Stop'

$Repo = "hooklistener/hooklistener-cli"
$BinaryName = "hooklistener.exe"
$DefaultInstallDir = "$env:LOCALAPPDATA\hooklistener"

function Write-Info {
    param([string]$Message)
    Write-Host "info: " -ForegroundColor Blue -NoNewline
    Write-Host $Message
}

function Write-Success {
    param([string]$Message)
    Write-Host "success: " -ForegroundColor Green -NoNewline
    Write-Host $Message
}

function Write-Warn {
    param([string]$Message)
    Write-Host "warning: " -ForegroundColor Yellow -NoNewline
    Write-Host $Message
}

function Write-Error-Custom {
    param([string]$Message)
    Write-Host "error: " -ForegroundColor Red -NoNewline
    Write-Host $Message
    exit 1
}

function Get-LatestVersion {
    $Url = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $Response = Invoke-RestMethod -Uri $Url -UseBasicParsing
        return $Response.tag_name
    }
    catch {
        Write-Error-Custom "Failed to get latest version from GitHub: $_"
    }
}

function Get-ExpectedChecksum {
    param(
        [string]$ChecksumsPath,
        [string]$ArchiveName
    )

    $Content = Get-Content $ChecksumsPath
    foreach ($Line in $Content) {
        if ($Line -match "^([a-fA-F0-9]{64})\s+.*$([regex]::Escape($ArchiveName))$") {
            return $Matches[1]
        }
    }
    return $null
}

function Verify-Checksum {
    param(
        [string]$FilePath,
        [string]$ExpectedChecksum
    )

    $ActualChecksum = (Get-FileHash -Path $FilePath -Algorithm SHA256).Hash.ToLower()
    $ExpectedLower = $ExpectedChecksum.ToLower()

    if ($ActualChecksum -ne $ExpectedLower) {
        Write-Error-Custom "Checksum verification failed!`nExpected: $ExpectedLower`nActual: $ActualChecksum"
    }

    Write-Success "Checksum verified"
}

function Add-ToPath {
    param([string]$Directory)

    $CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")

    if ($CurrentPath -notlike "*$Directory*") {
        $NewPath = "$CurrentPath;$Directory"
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        $env:Path = "$env:Path;$Directory"
        Write-Info "Added $Directory to user PATH"
        return $true
    }
    return $false
}

function Install-HooklistenerCli {
    Write-Host ""
    Write-Info "Installing Hooklistener CLI..."

    # Determine install directory
    $InstallDir = if ($env:HOOKLISTENER_INSTALL_DIR) {
        $env:HOOKLISTENER_INSTALL_DIR
    } else {
        $DefaultInstallDir
    }

    # Get latest version
    $Version = Get-LatestVersion
    Write-Info "Latest version: $Version"

    # Archive name for Windows
    $ArchiveName = "hooklistener-cli.exe-x86_64-pc-windows-msvc.zip"
    $DownloadUrl = "https://github.com/$Repo/releases/download/$Version/$ArchiveName"
    $ChecksumsUrl = "https://github.com/$Repo/releases/download/$Version/SHA256SUMS.txt"

    # Create temp directory
    $TempDir = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "hooklistener-install-$(Get-Random)")

    try {
        $ArchivePath = Join-Path $TempDir $ArchiveName
        $ChecksumsPath = Join-Path $TempDir "SHA256SUMS.txt"

        # Download archive
        Write-Info "Downloading $ArchiveName..."
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $ArchivePath -UseBasicParsing

        # Download checksums
        Write-Info "Downloading checksums..."
        Invoke-WebRequest -Uri $ChecksumsUrl -OutFile $ChecksumsPath -UseBasicParsing

        # Verify checksum
        Write-Info "Verifying checksum..."
        $ExpectedChecksum = Get-ExpectedChecksum -ChecksumsPath $ChecksumsPath -ArchiveName $ArchiveName
        if ($ExpectedChecksum) {
            Verify-Checksum -FilePath $ArchivePath -ExpectedChecksum $ExpectedChecksum
        } else {
            Write-Warn "Could not find checksum for $ArchiveName, skipping verification"
        }

        # Extract archive
        Write-Info "Extracting archive..."
        $ExtractDir = Join-Path $TempDir "extracted"
        Expand-Archive -Path $ArchivePath -DestinationPath $ExtractDir -Force

        # Create install directory
        if (-not (Test-Path $InstallDir)) {
            New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        }

        # Copy binary (rename from hooklistener-cli.exe to hooklistener.exe)
        Write-Info "Installing to $InstallDir..."
        $SourceBinary = Join-Path $ExtractDir "hooklistener-cli.exe"
        $DestBinary = Join-Path $InstallDir $BinaryName
        Copy-Item -Path $SourceBinary -Destination $DestBinary -Force

        # Add to PATH
        $PathUpdated = Add-ToPath -Directory $InstallDir

        # Verify installation
        if (Test-Path $DestBinary) {
            Write-Host ""
            Write-Host "Hooklistener CLI installed successfully!" -ForegroundColor Green
            Write-Host ""
            Write-Host "  Version:  $Version"
            Write-Host "  Location: $DestBinary"
            Write-Host ""
            Write-Host "Get started:" -ForegroundColor White
            Write-Host "  hooklistener tui      # Launch the terminal UI"
            Write-Host "  hooklistener login    # Authenticate with your account"
            Write-Host "  hooklistener --help   # View all commands"
            Write-Host ""

            if ($PathUpdated) {
                Write-Host "Note: " -ForegroundColor Yellow -NoNewline
                Write-Host "PATH was updated. Restart your terminal for changes to take effect."
                Write-Host ""
            }
        } else {
            Write-Error-Custom "Installation failed - binary not found at $DestBinary"
        }
    }
    finally {
        # Cleanup temp directory
        if (Test-Path $TempDir) {
            Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

# Run installation
Install-HooklistenerCli
