param(
    [string]$Target = "aarch64-linux-android"
)

$ErrorActionPreference = "Stop"
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$jniDirectory = Join-Path $PSScriptRoot "app\src\main\jniLibs\arm64-v8a"

New-Item -ItemType Directory -Force -Path $jniDirectory | Out-Null
Push-Location $projectRoot
try {
    cargo build --release --package remoteapp-android --lib --target $Target
    Copy-Item "target\$Target\release\libremoteapp_android.so" `
        (Join-Path $jniDirectory "libremoteapp_android.so") -Force
}
finally {
    Pop-Location
}
