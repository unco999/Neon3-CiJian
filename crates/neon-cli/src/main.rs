//! Public protocol client. It must not create windows or GPU objects.

use std::net::TcpListener;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--help"] {
        println!(
            "neon-cli scenario <ui.static-fragment.submit.v1|ui.detail-toggle.v1> --headless\n{}\n{}",
            neon_cli::debug_usage(),
            neon_cli::event_usage()
        );
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|command| command == "debug") {
        let command = match neon_cli::DebugCommand::parse(&args) {
            Ok(command) => command,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        return match neon_cli::execute_debug(command) {
            Ok(output) => {
                println!("{output}");
                if output["response"]["status"] == "accepted" {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"status": "failed", "error": {"code": "transport_failed", "message": error.to_string()}})
                );
                ExitCode::from(1)
            }
        };
    }
    if args.first().is_some_and(|command| command == "event") {
        let command = match neon_cli::EventCommand::parse(&args) {
            Ok(command) => command,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        return match neon_cli::execute_event(command) {
            Ok(output) => {
                println!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"status": "failed", "error": {"code": "transport_failed", "message": error.to_string()}})
                );
                ExitCode::from(1)
            }
        };
    }
    let scenario = match args.as_slice() {
        [command, scenario, mode]
            if command == "scenario"
                && mode == "--headless"
                && matches!(
                    scenario.as_str(),
                    "ui.static-fragment.submit.v1" | "ui.detail-toggle.v1"
                ) =>
        {
            scenario.as_str()
        }
        _ => {
            eprintln!("unsupported command");
            return ExitCode::from(2);
        }
    };

    let reservation = TcpListener::bind("127.0.0.1:0").expect("must reserve loopback endpoint");
    let endpoint = reservation
        .local_addr()
        .expect("reserved endpoint must have address");
    drop(reservation);
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let request_count = if scenario == neon_cli::DETAIL_TOGGLE_SCENARIO_ID {
        "5"
    } else {
        "10"
    };
    let mut server = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "-p",
            "neon-wgpu-runtime",
            "--",
            "--headless-server",
            &endpoint.to_string(),
            request_count,
        ])
        .current_dir(workspace)
        .spawn()
        .expect("must start headless WGPU runtime");

    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        match if scenario == neon_cli::DETAIL_TOGGLE_SCENARIO_ID {
            neon_cli::run_detail_toggle_scenario(endpoint)
        } else {
            neon_cli::run_headless_scenario(endpoint)
        } {
            Ok(outcome) => break outcome,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = server.kill();
                eprintln!(
                    "{}",
                    serde_json::json!({"scenario": scenario, "status": "failed", "error": error.to_string()})
                );
                return ExitCode::from(1);
            }
        }
    };
    let _ = server.wait();
    println!(
        "{}",
        serde_json::to_string(&outcome).expect("scenario JSON must serialize")
    );
    if outcome["status"] == "passed" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
