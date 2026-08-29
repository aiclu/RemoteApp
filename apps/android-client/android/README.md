# Android packaging

The Rust crate is the source of the native library. The Gradle project only packages that
library into an APK or App Bundle, so it does not introduce a Kotlin/Java application layer.

From the repository root, install the Android target and build the native library:

```text
rustup target add aarch64-linux-android
pwsh apps/android-client/android/build-rust.ps1
```

Then open `apps/android-client/android/` in Android Studio or run:

```text
gradlew.bat :app:bundleRelease
```

The current first release is arm64-only, uses Android API 26 as the minimum, and needs the
device's existing VPN/network route to reach the Windows RDP host. The Gradle wrapper is not
checked in yet; Android Studio can generate it for the chosen local Gradle distribution.
