//! Android host boundary for Neon3.
//!
//! This crate deliberately contains no Node.js, UI business state, window, or
//! GPU implementation. Those concerns are supplied by platform adapters while
//! control and event semantics remain the public Neon3 protocol.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    eprintln!(
        "{{\"probe\":\"android-host\",\"lifecycle\":\"created\",\"epoch\":1,\"surface_generation\":0}}"
    );
    if let Err(error) = neon_wgpu_runtime::WindowedRuntime::run_android_host(app) {
        eprintln!(
            "{{\"probe\":\"android-host\",\"sequence\":1,\"pass_result\":false,\"error\":{:?}}}",
            error
        );
    } else {
        eprintln!(
            "{{\"probe\":\"android-host\",\"lifecycle\":\"stopped\",\"epoch\":1,\"surface_generation\":1}}"
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformCapabilities {
    pub platform: String,
    pub architecture: String,
    pub runtime_mode: String,
    pub transport: String,
    pub surface: String,
    pub gpu_backend: String,
    pub protocol: String,
    pub renderer_owner: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub application_id: String,
    pub protocol_version: String,
    pub renderer_library: String,
    pub transport: String,
    pub orientation: String,
    pub ui_fixture: String,
    pub endpoint: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            application_id: "com.neon3.androidruntime".to_owned(),
            protocol_version: "neon3.rpc/1".to_owned(),
            renderer_library: "neon-wgpu-runtime".to_owned(),
            transport: "loopback_tcp".to_owned(),
            orientation: "landscape".to_owned(),
            ui_fixture: "none".to_owned(),
            endpoint: "127.0.0.1:43100".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLifecycle {
    Created,
    SurfaceReady,
    Running,
    Suspended,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDiagnostics {
    pub host: String,
    pub lifecycle: HostLifecycle,
    pub epoch: u64,
    pub surface_generation: u64,
    pub renderer_owner: String,
    pub protocol: String,
}

impl HostDiagnostics {
    pub fn new(lifecycle: HostLifecycle, epoch: u64, surface_generation: u64) -> Self {
        Self {
            host: "neon-android-host".to_owned(),
            lifecycle,
            epoch,
            surface_generation,
            renderer_owner: "neon-wgpu-runtime".to_owned(),
            protocol: "neon3.rpc/1".to_owned(),
        }
    }
}

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        runtime_mode: "embedded_host".to_owned(),
        transport: "loopback_tcp".to_owned(),
        surface: if cfg!(target_os = "android") {
            "android_native_window"
        } else {
            "not_available"
        }
        .to_owned(),
        gpu_backend: if cfg!(target_os = "android") {
            "vulkan"
        } else {
            "platform_selected"
        }
        .to_owned(),
        protocol: "neon3.rpc/1".to_owned(),
        renderer_owner: "neon-wgpu-runtime".to_owned(),
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_neon3_androidruntime_NeonRuntimeBridge_capabilitiesJson(
    env: jni::JNIEnv<'_>,
    _class: jni::objects::JClass<'_>,
) -> jni::sys::jstring {
    let json = serde_json::to_string(&capabilities()).expect("platform capabilities serialize");
    env.new_string(json)
        .expect("platform capabilities JNI string")
        .into_raw()
}

/// Start the headless protocol host from a Java Service. Never creates a
/// window or GPU surface; the process stays backgrounded until an SDK opens
/// a surface or sends `service.shutdown`. When the server thread exits (normal
/// shutdown or failure), `Neon3HostService.onHostServerStopped()` is invoked so
/// the Android foreground service can stop itself and the process can exit.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_neon3_androidruntime_Neon3HostService_hostStart(
    mut env: jni::JNIEnv<'_>,
    _class: jni::objects::JClass<'_>,
    endpoint: jni::objects::JString<'_>,
) -> jni::sys::jint {
    let endpoint: String = env
        .get_string(&endpoint)
        .map(|value| value.into())
        .unwrap_or_default();
    let endpoint: std::net::SocketAddr = match endpoint.parse() {
        Ok(address) => address,
        Err(_) => {
            eprintln!(
                "{{\"probe\":\"android-host-service\",\"pass_result\":false,\"error\":\"invalid_endpoint\"}}"
            );
            return 2;
        }
    };
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(_) => return 4,
    };
    // Cache the service class as a global reference on the calling (Java)
    // thread. A native thread that later attaches cannot resolve application
    // classes through FindClass (system class loader only), so the class must
    // be captured before spawning the server thread.
    let host_class = match env.find_class("com/neon3/androidruntime/Neon3HostService") {
        Ok(class) => match env.new_global_ref(class) {
            Ok(reference) => reference,
            Err(_) => return 5,
        },
        Err(_) => return 5,
    };
    let started = std::thread::Builder::new()
        .name("neon3-android-headless-host".into())
        .spawn(move || {
            let endpoint_owned = endpoint;
            // Run the GPU-backed headless server: it owns a wgpu device and can
            // export shared surface textures and PNG captures, while still
            // serving service.* / wgpu.* / debug.* methods on the single
            // endpoint. The join blocks until `service.shutdown`.
            let server = neon_wgpu_runtime::spawn_headless_external_server(endpoint_owned);
            let result = match server.join() {
                Ok(inner) => inner,
                Err(_) => Err("headless external server thread panicked".to_owned()),
            };
            if let Err(error) = result {
                eprintln!(
                    "{{\"probe\":\"android-host-service\",\"lifecycle\":\"failed\",\"endpoint\":\"{}\",\"error\":{error:?}}}",
                    endpoint_owned
                );
            }
            // Notify the Java service so it can stopSelf(). Attaching a JNI env
            // to this native thread is safe: hostStart was called from the
            // Java main thread, and JavaVM is process-global.
            if let Ok(mut attached) = vm.attach_current_thread() {
                let _ = attached.call_static_method(
                    &host_class,
                    "onHostServerStopped",
                    "()V",
                    &[],
                );
                // AttachGuard detaches on drop.
            }
        });
    match started {
        Ok(_) => 0,
        Err(_) => 3,
    }
}

/// Stop the headless host cleanly from the Java Service lifecycle.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_neon3_androidruntime_Neon3HostService_hostStop(
    mut _env: jni::JNIEnv<'_>,
    _class: jni::objects::JClass<'_>,
    endpoint: jni::objects::JString<'_>,
) {
    let endpoint: String = _env
        .get_string(&endpoint)
        .map(|value| value.into())
        .unwrap_or_default();
    let Ok(endpoint) = endpoint.parse::<std::net::SocketAddr>() else {
        return;
    };
    let request = neon_protocol::RpcRequest {
        protocol: "neon3.rpc".into(),
        version: neon_protocol::PROTOCOL_VERSION,
        request_id: neon_protocol::RequestId("android-host-service-stop".into()),
        client: neon_protocol::ClientIdentity {
            kind: neon_protocol::ClientKind::ExternalHost,
            instance_id: "neon3-android-host-service".into(),
            pid: std::process::id(),
            origin: "neon-android-host".into(),
        },
        target: neon_protocol::ServiceName("wgpu-runtime".into()),
        method: "service.shutdown".into(),
        params: serde_json::json!({}),
        expected_revision: None,
        idempotency_key: Some("android-host-service-stop".into()),
    };
    let _ = neon_ipc::RpcClient::connect(endpoint).and_then(|mut client| client.call(&request));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_serializable_without_language_specific_fields() {
        let value = serde_json::to_value(capabilities()).expect("capabilities serialize");
        assert!(value.get("node").is_none());
        assert!(value.get("gpu_handle").is_none());
    }

    #[test]
    fn diagnostics_identify_host_and_invalidate_surface_generations() {
        let diagnostics = HostDiagnostics::new(HostLifecycle::SurfaceReady, 7, 3);
        assert_eq!(diagnostics.host, "neon-android-host");
        assert_eq!(diagnostics.epoch, 7);
        assert_eq!(diagnostics.surface_generation, 3);
        assert_eq!(diagnostics.protocol, "neon3.rpc/1");
    }
}
