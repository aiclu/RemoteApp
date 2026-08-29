# RemoteAPP

RemoteAPP is an experimental Rust-first RDP client for Android phones and tablets.

The first release targets Android 8/API 26+, `arm64-v8a`, and Windows 10/11 hosts reached
through a system VPN. It uses IronRDP for protocol/session handling and Slint for the Rust UI.

## Current scope

- Direct TCP RDP over the device's existing network/VPN.
- NLA/CredSSP with username and password.
- The Android connection field accepts `host`, `host:port`, or bracketed IPv6 such as
  `[2001:db8::10]:3389`; the port defaults to 3389.
- Strict TLS validation with trust-on-first-use fingerprint support.
- One active session, touchpad input, hardware keyboard/mouse input, dynamic resize, and
  bounded automatic reconnect.
- Bidirectional Unicode text clipboard through a pluggable clipboard bridge.
- Encrypted local profile cache and optional direct PostgreSQL synchronization.
- No hosted service, account system, RD Gateway, file redirection, drive redirection, audio,
  multi-monitor, or remote Android control.

The current Android/desktop preview keeps the connection form and active session in memory. The
portable encrypted envelope and `KeyStorage` abstraction are implemented, but the Android
Keystore JNI adapter and profile-cache UI are intentionally the next integration step.

## Build the Rust workspace

```text
cargo test --workspace
cargo check --workspace
cargo fmt --all -- --check
```

The current checkout has Rust installed but does not have Java, ADB, or an Android target.
Install Android SDK/NDK and the target before building the app:

```text
rustup target add aarch64-linux-android
cargo install cargo-apk
cargo apk run -p remoteapp-android --target aarch64-linux-android --lib
```

For release packaging, use the Gradle project under `apps/android-client/android/` and produce
an Android App Bundle.

## GitHub Actions packaging

`.github/workflows/android.yml` runs formatting, workspace tests, a Rust compile, and an arm64
Debug APK build for pull requests, pushes, and manual runs. Pushing a tag such as `v0.1.0` also
builds a signed Release AAB. Configure these repository secrets before pushing a release tag:

- `ANDROID_KEYSTORE_BASE64`
- `ANDROID_KEYSTORE_PASSWORD`
- `ANDROID_KEY_ALIAS`
- `ANDROID_KEY_PASSWORD`

## Security notes

- RDP and PostgreSQL passwords never appear in serialized profile metadata or diagnostic logs.
- PostgreSQL credentials are local-only; the client must not connect to a database without TLS.
- Synced profile payloads use a versioned Argon2id + XChaCha20-Poly1305 envelope.
- The master password is not recoverable. Losing it requires re-entering RDP passwords.

## Project layout

- `crates/rdp-core`: platform-neutral profile, session, input, frame, and RDP adapter types.
- `crates/crypto-store`: versioned profile encryption and in-memory secret handling.
- `crates/sync-pg`: PostgreSQL schema, migrations, and optimistic conflict handling.
- `apps/android-client`: Slint UI, Android entry point, touchpad mapper, and renderer boundary.

The Android UI embeds Noto Sans SC for reliable Simplified Chinese glyph coverage; its license is
kept next to the font at `apps/android-client/ui/NotoSansSC-OFL.txt`.
