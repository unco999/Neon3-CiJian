# Neon3 Android Runtime

This is the first installable Android host shell. It targets Android API 37,
keeps the control contract language-neutral, and loads `neon-android-host` as
the NativeActivity entry. The Host owns Android lifecycle, Surface, input, and
bootstrap; it links the sole `neon-wgpu-runtime` renderer internally. Kotlin,
Node, Rust, or another SDK connects through `neon3.rpc` and does not link to
renderer or domain internals.

## Build the Rust library

From `D:\Neon3` after installing the Rust Android target and NDK:

```powershell
$env:ANDROID_HOME = "E:\AndroidSdk"
$env:ANDROID_NDK_HOME = "E:\AndroidSdk\ndk\30.0.16138531"
cargo ndk -t arm64-v8a -o examples/android-runtime/app/src/main/jniLibs build --release -p neon-android-host
```

## Build and install

Open this directory in Android Studio and run the `app` configuration, or use
the generated Gradle wrapper when available:

```powershell
.gradlew.bat :app:assembleDebug
& "E:\AndroidSdk\platform-tools\adb.exe" install -r app\build\outputs\apk\debug\app-debug.apk
```

The app writes one JSONL-compatible record to logcat:

```powershell
& "E:\AndroidSdk\platform-tools\adb.exe" logcat -s Neon3Probe:I "*:S"
```

The native host emits lifecycle records tagged `android-host`, including
`epoch` and `surface_generation`. A client must discard stale revisions after a
surface recreation or process restart.

## Generic host mode

The APK does not load `component-gallery` by default. After the NativeActivity
starts, the host exposes the loopback control endpoint:

```text
127.0.0.1:43100
```

An SDK connects to this endpoint using the public `neon3.rpc/1` envelope. The
SDK submits a flow or fragment, receives the runtime snapshot/events, and owns
the application/domain state. The host does not infer an application from the
APK and does not require Node.js.
