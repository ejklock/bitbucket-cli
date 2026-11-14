#Requires -Version 5.1
<#
.SYNOPSIS
    Install the bitbucket CLI for Windows from the ejklock/bitbucket-cli releases.
.PARAMETER Version
    Release tag to install (for example "v1.2.1"). Defaults to the latest release.
.EXAMPLE
    irm https://raw.githubusercontent.com/ejklock/bitbucket-cli/HEAD/install.ps1 | iex
.EXAMPLE
    .\install.ps1 -Version v1.2.1
#>
param(
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$ReleasesUrl = "https://github.com/ejklock/bitbucket-cli/releases"
$Asset = "bitbucket-windows-amd64.exe.zip"
$BinDir = Join-Path $env:LOCALAPPDATA "Programs\bitbucket"
$Dest = Join-Path $BinDir "bitbucket.exe"

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    Write-Error "Unsupported architecture: $($env:PROCESSOR_ARCHITECTURE). Only windows-amd64 is built; see $ReleasesUrl"
    exit 1
}

if ($Version -eq "") {
    $DownloadUrl = "$ReleasesUrl/latest/download/$Asset"
} else {
    $DownloadUrl = "$ReleasesUrl/download/$Version/$Asset"
}

$Work = Join-Path ([System.IO.Path]::GetTempPath()) ("bitbucket-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Work | Out-Null

try {
    $Zip = Join-Path $Work $Asset
    Write-Host "Downloading $DownloadUrl"
    $previousProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $Zip -UseBasicParsing
    } finally {
        $ProgressPreference = $previousProgress
    }

    $Extract = Join-Path $Work "extract"
    Expand-Archive -Path $Zip -DestinationPath $Extract
    $Exe = Join-Path $Extract "bitbucket.exe"
    if (-not (Test-Path $Exe)) {
        throw "The archive does not contain bitbucket.exe."
    }

    if (-not (Test-Path $BinDir)) {
        New-Item -ItemType Directory -Path $BinDir | Out-Null
    }
    Copy-Item -Path $Exe -Destination $Dest -Force
} catch {
    Write-Error "Install failed: $_"
    exit 1
} finally {
    Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
}

# Read the raw user PATH (no %VAR% expansion) so existing entries are written back unchanged.
$envKey = Get-Item -Path "HKCU:\Environment"
$rawPath = $envKey.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$wanted = $BinDir.TrimEnd('\')
$present = $false
foreach ($entry in $rawPath.Split(';')) {
    if ($entry.TrimEnd('\') -ieq $wanted) {
        $present = $true
        break
    }
}
if (-not $present) {
    if ($rawPath -eq "") {
        $newPath = $BinDir
    } else {
        $newPath = "$BinDir;$rawPath"
    }
    Set-ItemProperty -Path "HKCU:\Environment" -Name "Path" -Value $newPath -Type ExpandString
    Write-Host "Added $BinDir to your user PATH."
    # Tell running programs (Explorer, so new terminals) that the environment changed.
    try {
        Add-Type -Namespace BitbucketInstall -Name NativeMethods -MemberDefinition @"
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
"@
        $HWND_BROADCAST = [IntPtr]0xffff
        $WM_SETTINGCHANGE = 0x001A
        $SMTO_ABORTIFHUNG = 0x0002
        $result = [UIntPtr]::Zero
        $sent = [BitbucketInstall.NativeMethods]::SendMessageTimeout($HWND_BROADCAST, $WM_SETTINGCHANGE, [UIntPtr]::Zero, "Environment", $SMTO_ABORTIFHUNG, 5000, [ref]$result)
        if ($sent -eq [IntPtr]::Zero) { throw "SendMessageTimeout returned 0" }
    } catch {
        Write-Warning "Could not notify Windows of the PATH change: $_"
    }
    Write-Host "Restart your terminal (or open a new one) for it to take effect."
}

Write-Host "Installed to $Dest"
& $Dest --version
