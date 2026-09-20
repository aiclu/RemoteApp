# RemoteAPP

Rust + Slint RDP client for Android phones and tablets, with a Windows desktop preview.
The device-management and session flows take inspiration from the Android and iOS/iPadOS
versions of Microsoft Windows App. This project currently builds an **Android client**, not an iOS client.

## Implemented flows

- Device home with search, favorites, editing and confirmed deletion.
- Separate, scrollable connection editor; host, port, account, remote resolution, scaling and input mode.
- Full-window session with keyboard, mouse/direct-touch modes, pan/reset, clipboard and disconnect controls.
- Relative mouse movement, direct-touch coordinate mapping through the displayed image transform,
  long-press right-click, double-tap-and-hold drag, two-finger scrolling and pinch zoom.
- Hardware key forwarding and common Windows shortcuts; software keyboard text uses committed
  TextInput content rather than IME preedit. Ctrl/Alt/Win buttons toggle modifiers.
- Dynamic desktop resize after a short debounce; key release on cancellation, focus loss and disconnect.
- One active session; each connection owns its event stream and latest-frame slot.
  Old streams are discarded on switching/disconnecting. The UI samples only the newest frame at 30 Hz.
- Background display updates pause, retaining at most one pending frame for resume.
- Explicit Unicode text clipboard send/request plus Android system clipboard read/write.
- Light/dark appearance switch and responsive phone/tablet layouts.

## Storage and certificate trust

Device profiles are encrypted with the existing versioned XChaCha20-Poly1305 envelope and saved
atomically in the app-private directory. Android Keystore wraps the randomly generated vault key;
Windows preview uses the current user's DPAPI under %LOCALAPPDATA%/RemoteAPP.
There is no production fallback to MemoryKeyStorage. Missing/corrupt keys or files produce an error
instead of overwriting the original device data.

Passwords are **not saved by default**. Enable the per-device option to include a password inside the
encrypted catalog. Unchecking it removes the stored password on save. Temporary session credentials
are released on disconnect, except while an explicit certificate confirmation is pending.

Certificate exceptions are scoped to normalized host + port. Untrusted certificates require explicit
fingerprint confirmation; a saved fingerprint is enforced even if a replacement certificate is CA-valid.
Changed certificates show a separate warning. Deleting the last profile for an endpoint removes its
saved trust record.

## Protocol scope

Direct TCP RDP via the device's existing network/VPN, NLA/CredSSP username/password authentication,
bounded auto-reconnect and Unicode text clipboard. Native Windows touch/pen protocol redirection is
not implemented: direct touch maps to mouse input. No Microsoft cloud/workspace sign-in, RD Gateway,
audio, drive/file redirection, multi-monitor, or concurrent sessions.

The optional crypto-store and sync-pg libraries remain in the workspace. PostgreSQL synchronization
is not connected to this UI. On platforms other than Windows/Android, the preview reports that secure
storage is unavailable rather than storing secrets insecurely.

## Validation

```text
cargo test --workspace
cargo check --workspace
cargo fmt --all -- --check
```

Application tests cover stale sessions, cancellation, disconnected commands, latest-frame retention,
background resume, coordinate transforms, gesture sequences, committed Unicode deltas, encrypted
profile reload, password opt-in and storage failure. A headless renderer writes phone/tablet layout
screenshots to target/ui-review/. This is layout inspection, not a replacement for real-device tests.

Android build and packaging instructions: [Android README](apps/android-client/android/README.md).

Current local validation limitations: the Android Rust target is installed, but this workstation has
no configured Android SDK/NDK/JDK/ADB. An arm64 build attempt stops at missing NDK Clang. APK/AAB
packaging, Keystore/IME behavior on Android, real Bluetooth input and live RDP connectivity therefore
require validation on an Android-equipped machine. No successful device/network test is implied.

## GitHub Actions packaging

.github/workflows/android.yml runs formatting, workspace tests, a Rust compile, and an arm64
Debug APK build. Tags such as v0.1.0 also build a signed Release AAB with:

- ANDROID_KEYSTORE_BASE64
- ANDROID_KEYSTORE_PASSWORD
- ANDROID_KEY_ALIAS
- ANDROID_KEY_PASSWORD

## Project layout

- crates/rdp-core: protocol, input and session boundaries.
- crates/crypto-store: authenticated encryption and vault keys.
- crates/sync-pg: optional PostgreSQL synchronization library.
- apps/android-client: Slint UI, device repository, session controller, input mapping and OS services.
- vendor/ironrdp-tls: strict validation and endpoint fingerprint callback support.
- vendor/i-slint-backend-android-activity: Slint 1.17.1 with an Android mouse-wheel panic fix.

Noto Sans SC is embedded for Simplified Chinese; its license is alongside the font.
