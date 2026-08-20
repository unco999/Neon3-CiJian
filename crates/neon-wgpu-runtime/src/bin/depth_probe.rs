//! Standalone JSONL probe for the external world-UI depth pipeline.

use std::{env, io, net::SocketAddr, thread, time::Duration};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, ServiceName,
};
use serde_json::json;

fn request(endpoint: SocketAddr, sequence: u64) -> Result<serde_json::Value, String> {
    let rpc = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("depth-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "neon3-depth-probe".into(),
            pid: std::process::id(),
            origin: "neon3-depth-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: "render.depth_probe".into(),
        params: json!({}),
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("depth-probe-{sequence}")),
    };
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&rpc))
        .map_err(|error| error.to_string())?;
    if response.status != neon_protocol::RpcStatus::Accepted {
        return Err(format!("depth probe rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or_else(|| json!({})))
}

fn main() -> io::Result<()> {
    let mut args = env::args().skip(1);
    let endpoint = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:39103".into())
        .parse::<SocketAddr>()
        .map_err(io::Error::other)?;
    let mode = args.next().unwrap_or_else(|| "snapshot".into());
    let count = args
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);

    match mode.as_str() {
        "snapshot" => println!("{}", request(endpoint, 1).map_err(io::Error::other)?),
        "watch" => {
            for sequence in 1..=count {
                let result = request(endpoint, sequence).map_err(io::Error::other)?;
                println!("{}", json!({"callback": "depth.frame", "sequence": sequence, "data": result}));
                thread::sleep(Duration::from_millis(100));
            }
        }
        _ => {
            return Err(io::Error::other(
                "usage: depth_probe [endpoint] [snapshot|watch] [count]",
            ));
        }
    }
    Ok(())
}
