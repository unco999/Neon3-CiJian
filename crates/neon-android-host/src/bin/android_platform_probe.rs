use neon_android_host::{capabilities, HostConfig, HostDiagnostics, HostLifecycle};
use serde_json::json;

fn main() {
    let capabilities = capabilities();
    // The probe is also run on the development host. It verifies that the
    // platform description is protocol-shaped; an Android build will report
    // `platform: android` without changing the wire contract.
    let config = HostConfig::default();
    let diagnostics = HostDiagnostics::new(HostLifecycle::Running, 1, 1);
    let result = !capabilities.platform.is_empty()
        && !capabilities.architecture.is_empty()
        && capabilities.transport == config.transport
        && capabilities.protocol == config.protocol_version;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "probe": "android-platform-contract",
            "host": "neon-android-host",
            "input": {"target": std::env::consts::ARCH},
            "sequence": 1,
            "producer": capabilities,
            "host_config": config,
            "diagnostics": diagnostics,
            "consumer": {"protocol": "neon3.rpc", "language": "language_neutral"},
            "pass_result": result,
        }))
        .expect("probe JSON is serializable")
    );
    if !result {
        std::process::exit(2);
    }
}
