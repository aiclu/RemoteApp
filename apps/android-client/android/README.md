# Android packaging

Use this Gradle project to package the Rust library **and** the RemoteActivity Java bridge.
The bridge provides Android Keystore encryption and explicit system clipboard access.
A cargo-apk-only package does not include this bridge and is not a supported release path.

Required: JDK 17, Android SDK platform/build tools 35, NDK 27.2.12479018, Gradle 8.11.1,
and the Rust aarch64-linux-android target. Set JAVA_HOME, ANDROID_HOME and ANDROID_NDK_ROOT.

On Windows, from the repository root:

```powershell
rustup target add aarch64-linux-android
pwsh apps/android-client/android/build-rust.ps1
gradle --project-dir apps/android-client/android :app:assembleDebug
```

There is no checked-in Gradle wrapper; use the specified installed Gradle version.
Release packaging uses :app:bundleRelease. Set ANDROID_KEYSTORE_PATH,
ANDROID_KEYSTORE_PASSWORD, ANDROID_KEY_ALIAS and ANDROID_KEY_PASSWORD locally.
For GitHub Actions, configure ANDROID_KEYSTORE_BASE64 instead of the path, plus the
same three password/alias secrets. Stable v* tags require these signing secrets.

Prerelease tags containing a hyphen (for example v0.2.0-alpha.1) publish a debug-signed
arm64 APK as a GitHub prerelease after tests and the Android build pass. Stable tags
publish the signed AAB. Each tag needs matching release notes in releases/<tag>.md.
Debug APKs are for testing only; signing identity may differ between builds.

The native library and Java activity must be packaged together. NativeActivity's content rectangle
and adjustResize handle system insets and the software keyboard. The application uses the device's
existing network/VPN. Android API 26+ and arm64-v8a are the supported targets.
