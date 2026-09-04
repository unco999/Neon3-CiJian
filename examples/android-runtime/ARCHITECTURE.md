# Neon3 Android Host Boundary

`neon-android-host` is the Android platform adapter. An application embeds or
loads this host and selects a Neon3 client SDK independently.

```text
Kotlin / Java / Node / Rust SDK
              |
        neon3.rpc + neon3.event
              |
neon-android-host (lifecycle, Surface, input, IME, bootstrap)
              |
neon-wgpu-runtime (sole WGPU/Vulkan owner and final compositor)
```

The host boundary exposes platform capabilities and a small `HostConfig`. It
does not expose Rust pointers, GPU handles, UI element IDs, domain memory, or
project file access. SDKs send typed protocol envelopes with request, revision,
idempotency, session, and epoch fields.

## Integration contract

An Android packaging project supplies `libneon_android_host.so` for each target
ABI and declares it as the `NativeActivity` library. The host links the renderer
internally. A future AAR may wrap this exact boundary without changing the
protocol or renderer.

Supported first-party ABI outputs:

- `arm64-v8a` for physical Android devices
- `x86_64` for emulator development

The current repository may use `component-gallery.nui` as a separate host
acceptance fixture, but it is not the Android Host entry and is not loaded by
the APK. Applications provide their own UI declaration and connect their own
domain services through the public protocol. The generic APK host listens on
the loopback endpoint declared by `HostConfig`; no fixture is selected by
default.

The host publishes structured lifecycle diagnostics with `epoch` and
`surface_generation`. A surface recreation or host restart invalidates the old
generation; clients must request a fresh snapshot before sending revisioned
commands.
