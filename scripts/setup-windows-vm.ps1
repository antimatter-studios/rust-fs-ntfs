# setup-windows-vm.ps1 -- one-time provisioning of the Windows ARM64 test VM
# (fs-windows-test-harness's QEMU guest) with what this crate's matrix needs
# on it.
#
# What this installs:
#   -. Defender exclusions for the paths the tools and the matrix write to.
#   0. winget -- App Installer, which the guest's Windows 11 IoT Enterprise
#      LTSC does not ship, pinned to a winget-cli release; and winget's
#      catalogue. See Install-Winget for why both are provisioned.
#   1. Rustlang.Rustup -- rustup itself.
#   2. stable-aarch64-pc-windows-gnullvm toolchain -- Rust without the
#      need for Microsoft Visual C++ Build Tools (saves ~3 GB vs MSVC
#      and avoids the VS install dance entirely).
#   3. MartinStorsjo.LLVM-MinGW.UCRT -- bundles
#      `aarch64-w64-mingw32-clang.exe`, the linker the gnullvm target
#      requires for build scripts that compile native code (proc-macro2,
#      quote, etc.). ~200 MB self-contained.
#   4. Git.Git -- git, which clones rust-img-vhd.
#   5. rust-img-vhd (antimatter-studios/rust-img-vhd) -- creates the
#      VHD wrapper used by the harness (Mount-DiskImage refuses raw
#      images, only VHD/VHDX/ISO; rust-img-vhd builds the wrapper).
#
# Why these specific components:
#   - We picked gnullvm over MSVC because MSVC pulls in 3+ GB of Visual
#     Studio Build Tools and requires accepting Microsoft's commercial
#     license. gnullvm uses LLVM tooling end-to-end which has a clean
#     license and ~10x smaller footprint.
#   - LLVM-MinGW (Martin Storsjo's distribution) is the canonical way
#     to get aarch64-w64-mingw32-clang on Windows ARM64. Rustup's
#     gnullvm target ships rust-lld but expects this clang for build
#     scripts.
#   - rust-img-vhd creates the GPT-partitioned wrapper that lets
#     Mount-DiskImage attach our raw .img -- Windows Mount-DiskImage
#     refuses raw images, only VHD/VHDX/ISO. See
#     `docs/chkdsk-findings.md` iter1-2 for why this wrapper is needed.
#
# Usage:
#   - From the host, through the guest's ssh:
#       chore vm:setup
#   - On the VM directly, as an administrator (provisioning App Installer
#     for all users needs one), naming IMG_VHD_REF and FS_CORE_REF:
#       powershell -ExecutionPolicy Bypass -File setup-windows-vm.ps1 -VhdRef v0.6.0 -CoreRef v0.3.8
#
# Idempotent: every step checks before installing, so re-running is safe.

param(
    [string]$Workdir = "$env:USERPROFILE\dev",
    # rust-img-vhd's tag and the rust-fs-core tag it path-depends on:
    # IMG_VHD_REF and FS_CORE_REF in chores.yml, the pins CI builds it at,
    # which setup-windows-vm.sh reads and passes.
    [Parameter(Mandatory = $true)][string]$VhdRef,
    [Parameter(Mandatory = $true)][string]$CoreRef
)

$ErrorActionPreference = "Continue"  # winget writes progress to stderr;
                                     # don't abort on those.
# Progress bars reach an SSH client as CLIXML, and slow Invoke-WebRequest
# down by an order of magnitude.
$ProgressPreference = "SilentlyContinue"

function Test-WingetPackage {
    param([string]$Id)
    # winget list returns 0 if package is installed, non-zero otherwise.
    # The output also lists packages that *match* the ID -- we need an
    # exact match, so grep for the precise Id in column 2.
    $listing = winget list --id $Id --exact 2>&1 | Out-String
    return $listing -match [regex]::Escape($Id)
}

function Install-IfMissing {
    param([string]$Id, [string]$Description)
    Write-Host "[setup] $Description ($Id)"
    if (Test-WingetPackage -Id $Id) {
        Write-Host "        already installed -- skipping"
        return
    }
    winget install $Id --accept-source-agreements --accept-package-agreements --silent 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
}

# ---------- 0. winget ----------------------------------------------------
# The harness's QEMU guest is Windows 11 IoT Enterprise LTSC, which ships
# without the Microsoft Store and so without App Installer, the package that
# carries winget. It is installed here from winget-cli's own GitHub release,
# every file pinned by SHA-256, provisioned for all users and then
# registered for this one: a per-user install alone can fail in the
# non-interactive SSH session this script runs in.
$WingetRelease = "v1.29.380"
$WingetFiles = [ordered]@{
    "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe.msixbundle" = "65dea9c01ce08ee7b763366b27c0e651f97db857c11ca9b9c301826c10092f2e"
    "DesktopAppInstaller_Dependencies.zip"                   = "ba875afe9d190f61218985ac0292a99d1db710bf93e13c68944ca9d89f0d82d1"
    "e53e159d00e04f729cc2180cffd1c02e_License1.xml"          = "bcb15118ec47df24e3e6013a7006147c3a15b3a8104ed660fe87c8b4ed01f485"
}

function Install-Winget {
    Write-Host "[setup] winget (App Installer $WingetRelease)"
    $apps = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps"
    if (($env:Path -split ";") -notcontains $apps) { $env:Path += ";$apps" }
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        Write-Host "        already installed: winget $(winget --version)"
    } else {
        $dir = Join-Path $env:TEMP "winget-$WingetRelease"
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        foreach ($name in $WingetFiles.Keys) {
            $path = Join-Path $dir $name
            if (-not (Test-Path $path) -or (Get-FileHash $path -Algorithm SHA256).Hash -ne $WingetFiles[$name]) {
                $url = "https://github.com/microsoft/winget-cli/releases/download/$WingetRelease/$name"
                Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $path
            }
            $hash = (Get-FileHash $path -Algorithm SHA256).Hash
            if ($hash -ne $WingetFiles[$name]) {
                throw "winget: $name has SHA-256 $hash, not the pinned $($WingetFiles[$name])"
            }
        }
        $arch = @{ "ARM64" = "arm64"; "AMD64" = "x64" }[$env:PROCESSOR_ARCHITECTURE]
        if (-not $arch) { throw "winget: no dependency packages for $env:PROCESSOR_ARCHITECTURE" }
        $deps = Join-Path $dir "deps"
        Expand-Archive -Force -Path (Join-Path $dir "DesktopAppInstaller_Dependencies.zip") -DestinationPath $deps
        $depPaths = @(Get-ChildItem (Join-Path $deps $arch) -Filter *.appx | ForEach-Object FullName)
        Add-AppxProvisionedPackage -Online `
            -PackagePath (Join-Path $dir "Microsoft.DesktopAppInstaller_8wekyb3d8bbwe.msixbundle") `
            -DependencyPackagePath $depPaths `
            -LicensePath (Join-Path $dir "e53e159d00e04f729cc2180cffd1c02e_License1.xml") | Out-Null
        # A provisioned package registers at each user's next logon; this user
        # is logged on already.
        Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            throw "winget: App Installer is installed, but winget is not on PATH ($apps)"
        }
        Write-Host "        installed: winget $(winget --version)"
    }

    # THE CATALOGUE. winget's community source is a package too, which winget
    # installs for the current user on first use and refreshes the same way.
    # Over SSH that install fails with 0x80073D19 ("a user was logged off"),
    # and every search after it with 0x8a15000f ("data required by the
    # source is missing"). So it is provisioned and registered here, as App
    # Installer is, fresh on every run: it is data Microsoft signs and
    # republishes daily, so there is no hash to pin.
    $source = Join-Path $env:TEMP "winget-source2.msix"
    Invoke-WebRequest -UseBasicParsing -Uri "https://cdn.winget.microsoft.com/cache/source2.msix" -OutFile $source
    Add-AppxProvisionedPackage -Online -PackagePath $source -SkipLicense | Out-Null
    Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.Winget.Source_8wekyb3d8bbwe
    $catalogue = (Get-AppxPackage Microsoft.Winget.Source).Version
    if (-not $catalogue) { throw "winget: its catalogue (Microsoft.Winget.Source) did not register" }
    Write-Host "        catalogue: $catalogue"
}

# ---------- Defender exclusions -------------------------------------------
# Real-time scanning of every file the toolchains unpack, on the guest's two
# vCPUs, is most of their install time: winget unpacked LLVM-MinGW's ~9,000
# files at 1.6 a second with these paths scanned, and at 18 a second without
# (measured 2026-10-11 on the QEMU guest). The same paths take the matrix's
# disk images. This is a disposable test VM; nothing else on it is excluded.
Write-Host "[setup] Defender: excluding the test work paths from real-time scanning"
Add-MpPreference -ExclusionPath @(
    "$env:TEMP\WinGet",
    "$env:LOCALAPPDATA\Microsoft\WinGet",
    "$env:USERPROFILE\.cargo",
    "$env:USERPROFILE\.rustup",
    $Workdir,
    "C:\fswth"
)

Install-Winget

# ---------- 1-2. Rust + gnullvm toolchain --------------------------------
Install-IfMissing -Id "Rustlang.Rustup" -Description "Rustup (Rust installer)"

$cargoBin = "$env:USERPROFILE\.cargo\bin"
$env:PATH = "$cargoBin;$env:PATH"

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup not on PATH after install -- shell restart may be needed"
}

Write-Host "[setup] rustup default toolchain = stable-aarch64-pc-windows-gnullvm"
$current = (rustup show active-toolchain 2>&1) -replace '\s.*',''
if ($current -ne "stable-aarch64-pc-windows-gnullvm") {
    rustup default stable-aarch64-pc-windows-gnullvm 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
} else {
    Write-Host "        already set"
}

# A rust-toolchain.toml that names only a channel -- rust-img-vhd's names
# 1.95.0 -- resolves against rustup's DEFAULT HOST, not its default
# toolchain. winget's rustup sets aarch64-pc-windows-msvc, so that build
# picked 1.95.0-aarch64-pc-windows-msvc and failed on `linker link.exe not
# found`: MSVC is exactly what gnullvm is here to avoid.
Write-Host "[setup] rustup default host = aarch64-pc-windows-gnullvm"
rustup set default-host aarch64-pc-windows-gnullvm 2>&1 |
    Select-Object -Last 1 | ForEach-Object { Write-Host "        $_" }

# ---------- 3. LLVM-MinGW (aarch64-w64-mingw32-clang) --------------------
Install-IfMissing -Id "MartinStorsjo.LLVM-MinGW.UCRT" `
    -Description "LLVM-MinGW (linker for the gnullvm target)"

# ---------- 4. Workdir ---------------------------------------------------
$workdirPath = $Workdir.TrimEnd('\','/')
if (-not (Test-Path $workdirPath)) {
    New-Item -ItemType Directory -Path $workdirPath -Force | Out-Null
}
Write-Host "[setup] workdir: $workdirPath"

# ---------- 5. git, then rust-img-vhd (wrapper writer) --------------------
Install-IfMissing -Id "Git.Git" -Description "Git (clones rust-img-vhd)"
$gitCmd = "$env:ProgramFiles\Git\cmd"
if ((Test-Path $gitCmd) -and (($env:PATH -split ";") -notcontains $gitCmd)) { $env:PATH = "$gitCmd;$env:PATH" }
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "git not on PATH after install ($gitCmd)"
}

# Clones antimatter-studios/rust-img-vhd into the workdir AT ITS PINNED TAG,
# builds + installs its `rust-img-vhd` tool (the `cli` feature) to
# ~/.cargo/bin (which is on PATH after rustup setup). The harness's
# _lib.ps1::Initialize-VhdFromImg invokes `rust-img-vhd img <vhd> create`
# directly. We build it on the VM rather than cross-compile on the host so
# the binary matches the VM's gnullvm toolchain target and CPU arch without
# us maintaining a cross-build pipeline.
#
# It used to clone the default branch and pull it on every run. That is a
# different tool from the one CI builds, and rust-img-vhd path-depends on
# rust-fs-core (`../rust-fs-core`), which was never there, so it stopped
# building at all ("failed to read ...\rust-fs-core\Cargo.toml"). Now both
# are cloned at their pins, side by side, as CI's "Install rust-img-vhd"
# step does, in a directory of their own for each pair of pins.
$vhdPair = Join-Path $workdirPath "rust-img-vhd-$VhdRef-core-$CoreRef"
$vhdRepoDir = Join-Path $vhdPair "rust-img-vhd"
foreach ($sibling in @(
        @{ Name = "rust-img-vhd"; Ref = $VhdRef },
        @{ Name = "rust-fs-core"; Ref = $CoreRef })) {
    $dir = Join-Path $vhdPair $sibling.Name
    if (Test-Path $dir) {
        Write-Host "[setup] $($sibling.Name) $($sibling.Ref) already cloned"
        continue
    }
    Write-Host "[setup] cloning $($sibling.Name) $($sibling.Ref) into $dir"
    git clone --quiet --depth 1 --branch $sibling.Ref "https://github.com/antimatter-studios/$($sibling.Name).git" $dir 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
    if ($LASTEXITCODE -ne 0) { throw "git clone $($sibling.Name) $($sibling.Ref) failed (exit $LASTEXITCODE)" }
}

Write-Host "[setup] cargo install --path $vhdRepoDir --features cli --bin rust-img-vhd"
Push-Location $vhdRepoDir
cargo install --path . --features cli --bin rust-img-vhd 2>&1 |
    Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
$cargoExit = $LASTEXITCODE
Pop-Location
if ($cargoExit -ne 0) { throw "cargo install rust-img-vhd $VhdRef failed (exit $cargoExit)" }

# ---------- 6. Verify everything ----------------------------------------
$env:PATH = "$cargoBin;$env:PATH"

Write-Host ""
Write-Host "=== Verification ==="
& rustc --version 2>&1 | Select-Object -First 1
& cargo --version 2>&1 | Select-Object -First 1
$vhdToolBin = Get-Command rust-img-vhd -ErrorAction SilentlyContinue
if ($vhdToolBin) {
    Write-Host "rust-img-vhd: $($vhdToolBin.Source) ($(& $vhdToolBin.Source --version 2>&1 | Select-Object -First 1))"
} else {
    Write-Host "WARN: rust-img-vhd not on PATH; new shell may be needed"
}
$mingwClang = Get-Command "aarch64-w64-mingw32-clang*" -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($mingwClang) {
    Write-Host "aarch64-w64-mingw32-clang: $($mingwClang.Source)"
} else {
    Write-Host "WARN: aarch64-w64-mingw32-clang not on PATH; new shell may be needed"
}

Write-Host ""
Write-Host "=== Setup complete ==="
