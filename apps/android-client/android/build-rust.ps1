param([string]$Target = "aarch64-linux-android")
$ErrorActionPreference = "Stop"
if ($Target -ne "aarch64-linux-android") { throw "Only arm64-v8a packaging is configured." }
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "../../..")).Path
$ndkRoot = $env:ANDROID_NDK_ROOT
if (!$ndkRoot) { throw "Set ANDROID_NDK_ROOT to the installed Android NDK (27.2.12479018)." }
$toolchain = Join-Path $ndkRoot "toolchains/llvm/prebuilt/windows-x86_64/bin"
$linker = Join-Path $toolchain "aarch64-linux-android26-clang.cmd"
$cxx = Join-Path $toolchain "aarch64-linux-android26-clang++.cmd"
$ar = Join-Path $toolchain "llvm-ar.exe"
foreach ($tool in @($linker,$cxx,$ar)) { if (!(Test-Path -LiteralPath $tool)) { throw "Missing Android build tool: $tool" } }
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $linker
$env:CC_aarch64_linux_android = $linker
$env:CXX_aarch64_linux_android = $cxx
$env:AR_aarch64_linux_android = $ar
$jniDirectory = Join-Path $PSScriptRoot "app/src/main/jniLibs/arm64-v8a"
Push-Location $projectRoot
try {
    cargo build --release --package remoteapp-android --lib --target $Target --target-dir target
    if ($LASTEXITCODE -ne 0) { throw "Rust Android build failed ($LASTEXITCODE)." }
    New-Item -ItemType Directory -Force -Path $jniDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $projectRoot "target/$Target/release/libremoteapp_android.so") -Destination $jniDirectory -Force
} finally { Pop-Location }
