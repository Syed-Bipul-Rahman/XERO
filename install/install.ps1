# Xero installer for Windows.
#
#   irm https://download.xerolang.org/install.ps1 | iex
#
# Environment variables:
#   XERO_VERSION      release to install (default: latest), e.g. v0.1.0
#   XERO_INSTALL_DIR  install directory (default: %LOCALAPPDATA%\Programs\Xero)
#   XERO_NO_DEPS=1    do not install LLVM/clang
#   XERO_REPO         GitHub repository (default: Syed-Bipul-Rahman/XERO)
#   XERO_DOWNLOAD_BASE  base URL holding the archives and SHA256SUMS (mirrors)
#   XERO_ARCHIVE      install from a local .zip instead of downloading (for testing)
#   XERO_UNINSTALL=1  remove Xero

function Install-Xero {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = if ($env:XERO_REPO) { $env:XERO_REPO } else { 'Syed-Bipul-Rahman/XERO' }
    $version = if ($env:XERO_VERSION) { $env:XERO_VERSION } else { 'latest' }
    $root = if ($env:XERO_INSTALL_DIR) { $env:XERO_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Xero' }
    $bin = Join-Path $root 'bin'

    if ($env:XERO_UNINSTALL -eq '1') {
        Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
        Set-UserPath -Remove $bin
        Write-Ok "removed Xero from $root"
        return
    }

    # ------------------------------------------------------------ platform
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq 'ARM64') {
        Write-Warn 'no native ARM64 build yet; installing the x86-64 build (runs under emulation)'
    } elseif ($arch -ne 'AMD64') {
        throw "unsupported architecture: $arch"
    }
    $platform = 'windows-x86_64'
    $asset = "xeroc-$platform.zip"

    # ------------------------------------------------------------ download
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("xero-install-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp $asset
        if ($env:XERO_ARCHIVE) {
            Write-Ok "installing from local archive $env:XERO_ARCHIVE"
            Copy-Item $env:XERO_ARCHIVE $zip
        } else {
            $base = if ($env:XERO_DOWNLOAD_BASE) {
                $env:XERO_DOWNLOAD_BASE
            } elseif ($version -eq 'latest') {
                "https://github.com/$repo/releases/latest/download"
            } else {
                "https://github.com/$repo/releases/download/$version"
            }
            Write-Ok "downloading $asset ($version)"
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $zip
            $sums = Join-Path $tmp 'SHA256SUMS'
            $haveSums = $true
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums
            } catch {
                $haveSums = $false
                Write-Warn 'no SHA256SUMS published for this release; skipping checksum verification'
            }
            if ($haveSums) {
                $line = Get-Content $sums | Where-Object { $_ -match " $([Regex]::Escape($asset))$" } | Select-Object -First 1
                if ($line) {
                    $want = ($line -split '\s+')[0].ToLower()
                    $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
                    if ($want -ne $got) { throw "checksum mismatch for $asset (expected $want, got $got)" }
                    Write-Ok 'checksum verified'
                }
            }
        }

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $src = Join-Path $tmp "xeroc-$platform"
        if (-not (Test-Path (Join-Path $src 'xeroc.exe'))) { throw 'archive does not contain xeroc.exe' }

        # -------------------------------------------------------- install
        New-Item -ItemType Directory -Force -Path $bin | Out-Null
        Copy-Item (Join-Path $src 'xeroc.exe') (Join-Path $bin 'xeroc.exe') -Force
        foreach ($item in 'examples', 'docs', 'README.md', 'INSTALL.md') {
            $p = Join-Path $src $item
            if (Test-Path $p) {
                Remove-Item -Recurse -Force (Join-Path $root $item) -ErrorAction SilentlyContinue
                Copy-Item -Recurse $p (Join-Path $root $item)
            }
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
    Set-UserPath -Add $bin
    $ver = & (Join-Path $bin 'xeroc.exe') --version
    Write-Ok "installed $ver to $bin"

    # ------------------------------------------------------------ clang
    if ($env:XERO_NO_DEPS -ne '1') { Install-Toolchain }

    Write-Host ''
    Write-Ok 'Xero is ready. Open a new terminal and try:'
    Write-Host "    xeroc run `"$root\examples\hello.xr`""
    Write-Host ''
}

function Write-Ok($msg) { Write-Host "xero: $msg" -ForegroundColor Green }
function Write-Warn($msg) { Write-Host "xero: $msg" -ForegroundColor Yellow }

function Set-UserPath {
    param([string]$Add, [string]$Remove)
    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @()
    if ($current) { $parts = $current -split ';' | Where-Object { $_ -ne '' } }
    if ($Remove) { $parts = $parts | Where-Object { $_ -ne $Remove } }
    if ($Add -and ($parts -notcontains $Add)) { $parts = @($Add) + $parts }
    [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
    if ($Add -and (($env:Path -split ';') -notcontains $Add)) { $env:Path = "$Add;$env:Path" }
}

function Find-Clang {
    if ($env:XERO_CLANG -and (Test-Path $env:XERO_CLANG)) { return $env:XERO_CLANG }
    $cmd = Get-Command clang -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($p in "$env:ProgramFiles\LLVM\bin\clang.exe", "${env:ProgramFiles(x86)}\LLVM\bin\clang.exe") {
        if ($p -and (Test-Path $p)) { return $p }
    }
    return $null
}

function Install-Toolchain {
    # 1. clang: the LLVM back end that turns Xero's output into machine code
    $clang = Find-Clang
    if (-not $clang) {
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            Write-Ok 'installing LLVM/clang with winget (Xero uses it for code generation)'
            winget install --id LLVM.LLVM -e --silent --accept-package-agreements --accept-source-agreements | Out-Host
            $clang = Find-Clang
        }
    }
    if ($clang) {
        if (-not (Get-Command clang -ErrorAction SilentlyContinue)) {
            # LLVM's installer does not always add itself to PATH
            [Environment]::SetEnvironmentVariable('XERO_CLANG', $clang, 'User')
            $env:XERO_CLANG = $clang
        }
        Write-Ok "found clang: $clang"
    } else {
        Write-Warn 'clang not found. Install LLVM: winget install LLVM.LLVM  (or https://github.com/llvm/llvm-project/releases)'
    }

    # 2. the MSVC linker and C runtime (clang links against them on Windows)
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $hasMsvc = $false
    if (Test-Path $vswhere) {
        $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $hasMsvc = [bool]$found
    }
    if (-not $hasMsvc) {
        Write-Warn 'the Visual Studio C++ Build Tools (linker + C runtime) were not found. To build executables, install them with:'
        Write-Host '    winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
        Write-Host '  (xeroc can still produce objects, assembly and WebAssembly without them.)'
    }
}

Install-Xero
