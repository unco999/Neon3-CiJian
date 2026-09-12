//! Component showcase probe with full interactivity.
//!
//! Loads cases/component-showcase/showcase.nui and runs a persistent window
//! with a minimal UI-host RPC server. All component interactions are wired:
//! Button, Checkbox, Radio, Scroll buttons, ContextMenu, TreeView toggle.

use std::{
    net::SocketAddr,
    process::{Child, Command},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName, UiImageSource, UiImageUploadRequest,
};
use neon_ui_schema::{
    UiCommand, UiControlPresentation, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission,
    UiNode, UiNodeId, UiSemanticEvent, TextRef,
};
use neon_ui_runtime::nui_flow::{lower_nui_flow, lower_nui_flow_effects, parse_nui_flow};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39254";
const UI_ENDPOINT: &str = "127.0.0.1:39255";
const NUI_PATH: &str = r"D:\Neon3\cases\component-showcase\showcase.nui";

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("showcase-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "showcase-probe".into(),
            pid: std::process::id(),
            origin: "showcase-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("showcase-{sequence}")),
    }
}

fn call(
    endpoint: SocketAddr,
    method: &str,
    sequence: u64,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request(method, sequence, params)))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or_else(|| json!({})))
}

/// Generate a solid-color RGBA8 image (16x16, suitable for nine_slice).
fn solid_image(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    for _ in 0..16 * 16 {
        bytes.extend_from_slice(&[r, g, b, a]);
    }
    bytes
}

/// Generate a 16x16 image with a 2px border and inner fill (two-tone).
fn border_image(border: (u8, u8, u8, u8), fill: (u8, u8, u8, u8)) -> Vec<u8> {
    let (br, bg, bb, ba) = border;
    let (fr, fg, fb, fa) = fill;
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let is_border = x < 2 || x >= 14 || y < 2 || y >= 14;
            if is_border {
                bytes.extend_from_slice(&[br, bg, bb, ba]);
            } else {
                bytes.extend_from_slice(&[fr, fg, fb, fa]);
            }
        }
    }
    bytes
}

/// Generate a 16x16 filled circle (transparent outside).
fn circle_image(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    let cx = 7.5;
    let cy = 7.5;
    let radius = 7.0;
    for y in 0..16 {
        for x in 0..16 {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                bytes.extend_from_slice(&[r, g, b, a]);
            } else {
                bytes.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    bytes
}

/// Generate a 16x16 ring (circle with transparent center, 3px border).
fn ring_image(border: (u8, u8, u8, u8)) -> Vec<u8> {
    let (br, bg, bb, ba) = border;
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    let cx = 7.5;
    let cy = 7.5;
    let outer_r = 7.0;
    let inner_r = 4.0;
    for y in 0..16 {
        for x in 0..16 {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= outer_r && dist >= inner_r {
                bytes.extend_from_slice(&[br, bg, bb, ba]);
            } else {
                bytes.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    bytes
}

/// Generate a 16x16 diagonal striped image (45-degree stripes).
fn striped_image(stripe: (u8, u8, u8, u8), bg: (u8, u8, u8, u8)) -> Vec<u8> {
    let (sr, sg, sb, sa) = stripe;
    let (br, bg, bb, ba) = bg;
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let is_stripe = ((x + y) % 6) < 3;
            if is_stripe {
                bytes.extend_from_slice(&[sr, sg, sb, sa]);
            } else {
                bytes.extend_from_slice(&[br, bg, bb, ba]);
            }
        }
    }
    bytes
}

/// Generate a 16x16 rounded-rectangle image (transparent corners, 3px radius).
fn rounded_image(fill: (u8, u8, u8, u8)) -> Vec<u8> {
    let (fr, fg, fb, fa) = fill;
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    let radius = 3.0;
    for y in 0..16 {
        for x in 0..16 {
            // Determine if this pixel is inside the rounded rect
            let in_corner = if x < 3 && y < 3 {
                let dx = 2.5 - x as f32;
                let dy = 2.5 - y as f32;
                dx * dx + dy * dy > radius * radius
            } else if x >= 13 && y < 3 {
                let dx = x as f32 - 12.5;
                let dy = 2.5 - y as f32;
                dx * dx + dy * dy > radius * radius
            } else if x < 3 && y >= 13 {
                let dx = 2.5 - x as f32;
                let dy = y as f32 - 12.5;
                dx * dx + dy * dy > radius * radius
            } else if x >= 13 && y >= 13 {
                let dx = x as f32 - 12.5;
                let dy = y as f32 - 12.5;
                dx * dx + dy * dy > radius * radius
            } else {
                false
            };
            if in_corner {
                bytes.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                bytes.extend_from_slice(&[fr, fg, fb, fa]);
            }
        }
    }
    bytes
}

/// Upload a generated image to the runtime so skins can reference it. Retries on timeout.
fn upload_image(endpoint: SocketAddr, seq: u64, image_id: &str, bytes: Vec<u8>) -> Result<(), String> {
    let upload = UiImageUploadRequest {
        source: UiImageSource {
            image_id: image_id.into(),
            media_type: "application/x-neon-rgba8".into(),
            width: 16,
            height: 16,
            bytes,
        },
    };
    let mut last_err = String::new();
    for attempt in 0..5 {
        match call(endpoint, "wgpu.ui.image.upload", seq, serde_json::to_value(&upload).unwrap()) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = e;
                if attempt < 4 {
                    thread::sleep(Duration::from_millis(200 * (attempt + 1)));
                }
            }
        }
    }
    Err(format!("upload {image_id} failed after 5 attempts: {last_err}"))
}

/// Upload all showcase skin images. Called once before fragment submission.
fn upload_showcase_images(endpoint: SocketAddr) -> Result<(), String> {
    let mut seq = 1000u64;
    // Panel backgrounds
    upload_image(endpoint, seq, "panel-bg", solid_image(40, 50, 70, 230))?; seq += 1;
    upload_image(endpoint, seq, "panel-dark", solid_image(28, 28, 34, 240))?; seq += 1;
    upload_image(endpoint, seq, "panel-warm", solid_image(58, 42, 30, 235))?; seq += 1;
    upload_image(endpoint, seq, "panel-accent", solid_image(24, 58, 54, 235))?; seq += 1;
    // Button idle / hover
    upload_image(endpoint, seq, "btn-idle", solid_image(50, 90, 160, 255))?; seq += 1;
    upload_image(endpoint, seq, "btn-hover", solid_image(70, 120, 200, 255))?; seq += 1;
    upload_image(endpoint, seq, "btn-idle-dark", solid_image(60, 60, 72, 255))?; seq += 1;
    upload_image(endpoint, seq, "btn-hover-dark", solid_image(85, 85, 100, 255))?; seq += 1;
    upload_image(endpoint, seq, "btn-idle-warm", solid_image(160, 100, 50, 255))?; seq += 1;
    upload_image(endpoint, seq, "btn-hover-warm", solid_image(200, 130, 70, 255))?; seq += 1;
    // Slider / progress / input / tooltip / scrollbar
    upload_image(endpoint, seq, "track-bg", solid_image(40, 44, 52, 255))?; seq += 1;
    upload_image(endpoint, seq, "fill-bg", solid_image(80, 140, 220, 255))?; seq += 1;
    upload_image(endpoint, seq, "thumb-bg", solid_image(180, 200, 230, 255))?; seq += 1;
    upload_image(endpoint, seq, "input-bg", solid_image(30, 34, 42, 255))?; seq += 1;
    upload_image(endpoint, seq, "check-icon", solid_image(120, 200, 255, 255))?; seq += 1;
    upload_image(endpoint, seq, "radio-dot", solid_image(120, 200, 255, 255))?; seq += 1;
    upload_image(endpoint, seq, "tooltip-bg", solid_image(50, 48, 40, 245))?; seq += 1;
    upload_image(endpoint, seq, "scrollbar-track", solid_image(30, 32, 38, 200))?; seq += 1;
    upload_image(endpoint, seq, "scrollbar-thumb", solid_image(100, 108, 120, 220))?; seq += 1;
    upload_image(endpoint, seq, "progress-track", solid_image(40, 44, 52, 255))?; seq += 1;
    upload_image(endpoint, seq, "progress-fill", solid_image(80, 180, 120, 255))?; seq += 1;
    // Slider variants
    upload_image(endpoint, seq, "slider-fat-track", border_image((180, 100, 30, 255), (100, 60, 20, 255)))?; seq += 1;
    upload_image(endpoint, seq, "slider-fat-fill", solid_image(240, 180, 40, 255))?; seq += 1;
    upload_image(endpoint, seq, "slider-fat-thumb", border_image((255, 255, 255, 255), (220, 220, 230, 255)))?; seq += 1;
    upload_image(endpoint, seq, "slider-min-track", solid_image(60, 60, 68, 255))?; seq += 1;
    upload_image(endpoint, seq, "slider-min-fill", solid_image(80, 200, 140, 255))?; seq += 1;
    upload_image(endpoint, seq, "slider-min-thumb", solid_image(200, 255, 220, 255))?; seq += 1;
    // Checkbox variants
    upload_image(endpoint, seq, "check-dark-body", border_image((80, 80, 90, 255), (20, 20, 26, 255)))?; seq += 1;
    upload_image(endpoint, seq, "check-dark-icon", solid_image(240, 240, 250, 255))?; seq += 1;
    upload_image(endpoint, seq, "check-warm-body", border_image((140, 90, 40, 255), (50, 35, 20, 255)))?; seq += 1;
    upload_image(endpoint, seq, "check-warm-icon", solid_image(255, 200, 80, 255))?; seq += 1;
    // Progress variants
    upload_image(endpoint, seq, "progress-blue-fill", solid_image(60, 120, 220, 255))?; seq += 1;
    upload_image(endpoint, seq, "progress-warm-fill", solid_image(220, 130, 50, 255))?; seq += 1;
    // Input variants
    upload_image(endpoint, seq, "input-dark-bg", border_image((70, 70, 80, 255), (15, 15, 20, 255)))?; seq += 1;
    upload_image(endpoint, seq, "input-warm-bg", border_image((120, 80, 40, 255), (40, 28, 18, 255)))?; seq += 1;
    // Circle checkbox style
    upload_image(endpoint, seq, "check-circle-ring", ring_image((100, 160, 255, 255)))?; seq += 1;
    upload_image(endpoint, seq, "check-circle-dot", circle_image(100, 200, 255, 255))?; seq += 1;
    // Card checkbox style (thick border)
    upload_image(endpoint, seq, "check-card-body", border_image((180, 80, 200, 255), (40, 20, 50, 255)))?; seq += 1;
    upload_image(endpoint, seq, "check-card-icon", solid_image(220, 140, 255, 255))?; seq += 1;
    // Striped progress
    upload_image(endpoint, seq, "progress-striped-fill", striped_image((100, 200, 140, 255), (60, 140, 90, 255)))?; seq += 1;
    // Rounded progress
    upload_image(endpoint, seq, "progress-rounded-track", rounded_image((40, 44, 52, 255)))?; seq += 1;
    upload_image(endpoint, seq, "progress-rounded-fill", rounded_image((220, 100, 180, 255)))?; seq += 1;
    // Square slider thumb
    upload_image(endpoint, seq, "slider-square-thumb", border_image((255, 200, 60, 255), (200, 150, 30, 255)))?; seq += 1;
    upload_image(endpoint, seq, "slider-square-track", solid_image(50, 50, 60, 255))?; seq += 1;
    upload_image(endpoint, seq, "slider-square-fill", solid_image(255, 200, 60, 255))?; seq += 1;
    // Scrollbar variants
    upload_image(endpoint, seq, "scrollbar-fat-track", solid_image(50, 30, 20, 255))?; seq += 1;
    upload_image(endpoint, seq, "scrollbar-fat-thumb", border_image((255, 160, 40, 255), (220, 120, 20, 255)))?; seq += 1;
    upload_image(endpoint, seq, "scrollbar-dark-track", solid_image(18, 18, 24, 255))?; seq += 1;
    upload_image(endpoint, seq, "scrollbar-dark-thumb", solid_image(60, 100, 180, 220))?; seq += 1;
    // Radio variants
    upload_image(endpoint, seq, "radio-circle-ring", ring_image((100, 200, 140, 255)))?; seq += 1;
    upload_image(endpoint, seq, "radio-circle-dot", circle_image(100, 220, 160, 255))?; seq += 1;
    upload_image(endpoint, seq, "radio-card-body", border_image((80, 160, 200, 255), (20, 50, 70, 255)))?; seq += 1;
    upload_image(endpoint, seq, "radio-card-icon", solid_image(120, 200, 240, 255))?; seq += 1;
    // Tooltip variants
    upload_image(endpoint, seq, "tooltip-dark-bg", solid_image(20, 20, 28, 250))?; seq += 1;
    upload_image(endpoint, seq, "tooltip-accent-bg", border_image((60, 120, 220, 255), (30, 50, 90, 245)))?; seq += 1;
    println!("Uploaded {} skin images", seq - 1000);
    Ok(())
}

fn launch() -> std::io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .args(["--window-server", ENDPOINT, UI_ENDPOINT])
        .spawn()
}

fn load_nui_fragment() -> Result<(UiNode, Vec<UiEffect>), String> {
    let source = std::fs::read_to_string(NUI_PATH)
        .map_err(|error| format!("read nui failed: {error}"))?;
    let document = parse_nui_flow(&source)
        .map_err(|error| format!("parse nui failed: {error:?}"))?;
    let ir = lower_nui_flow(&document);
    let effects = lower_nui_flow_effects(&document);
    Ok((ir.root, effects))
}

// === Application state ===

struct TreeState {
    project_expanded: bool,
    crates_expanded: bool,
}

impl TreeState {
    fn new() -> Self { Self { project_expanded: true, crates_expanded: true } }
    fn toggle_project(&mut self) { self.project_expanded = !self.project_expanded; }
    fn toggle_crates(&mut self) { self.crates_expanded = !self.crates_expanded; }
    fn apply(&self, node: &mut UiNode) {
        match node.node_id.0.as_str() {
            "tree-root" => {
                if let Some(text) = &mut node.text {
                    *text = TextRef::Literal {
                        value: if self.project_expanded { "▼ project/" } else { "▶ project/" }.into(),
                    };
                }
            }
            "tree-crate-1" => {
                node.visible = self.project_expanded;
                if let Some(text) = &mut node.text {
                    *text = TextRef::Literal {
                        value: if self.crates_expanded { "▼ crates/" } else { "▶ crates/" }.into(),
                    };
                }
            }
            "tree-crate-2" | "tree-crate-3" | "tree-crate-4" => {
                node.visible = self.project_expanded && self.crates_expanded;
            }
            "tree-docs" => {
                node.visible = self.project_expanded;
                node.bounds.y = if self.crates_expanded { 148.0 } else { 64.0 };
            }
            "tree-cases" => {
                node.visible = self.project_expanded;
                node.bounds.y = if self.crates_expanded { 176.0 } else { 92.0 };
            }
            _ => {}
        }
        for child in &mut node.children {
            self.apply(child);
        }
    }
}

struct AppState {
    checkbox_state: bool,
    radio_state: bool,
    switch_state: bool,
    slider_val: f32,
    scroll_pos: f32,
    progress_val: f32,
    click_count: u32,
    context_menu_visible: bool,
    splitter_mode: u8, // 0=50/50, 1=30/70, 2=70/30
    skin_check_def: bool,
    skin_check_circle: bool,
    skin_check_card: bool,
    skin_radio_def: bool,
    skin_radio_circle: bool,
    skin_radio_card: bool,
    tree: TreeState,
}

impl AppState {
    fn new() -> Self {
        Self {
            checkbox_state: true,
            radio_state: false,
            switch_state: true,
            slider_val: 42.0,
            scroll_pos: 0.3,
            progress_val: 0.65,
            click_count: 0,
            context_menu_visible: false,
            splitter_mode: 0,
            skin_check_def: true,
            skin_check_circle: true,
            skin_check_card: false,
            skin_radio_def: true,
            skin_radio_circle: false,
            skin_radio_card: true,
            tree: TreeState::new(),
        }
    }

    /// Build ControlPresentation effects from current state.
    fn presentation_effects(&self) -> Vec<UiEffect> {
        vec![
            UiEffect::ControlPresentation {
                node_id: UiNodeId("check-demo".into()),
                state: UiControlPresentation::Toggle { selected: self.checkbox_state },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("radio-demo".into()),
                state: UiControlPresentation::Toggle { selected: self.radio_state },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("switch-demo".into()),
                state: UiControlPresentation::Toggle { selected: self.switch_state },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("slider-demo".into()),
                state: UiControlPresentation::Numeric { value: self.slider_val, min: 0.0, max: 100.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("progress-demo".into()),
                state: UiControlPresentation::Numeric { value: self.progress_val, min: 0.0, max: 1.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("scroll-demo".into()),
                state: UiControlPresentation::Scroll { position: self.scroll_pos },
            },
            // Skin variant sliders
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-slider-def".into()),
                state: UiControlPresentation::Numeric { value: self.slider_val, min: 0.0, max: 100.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-slider-fat".into()),
                state: UiControlPresentation::Numeric { value: self.slider_val, min: 0.0, max: 100.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-slider-min".into()),
                state: UiControlPresentation::Numeric { value: self.slider_val, min: 0.0, max: 100.0 },
            },
            // Skin variant checkboxes (independent states)
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-check-def".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_check_def },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-check-circle".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_check_circle },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-check-card".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_check_card },
            },
            // Skin variant progress bars
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-prog-def".into()),
                state: UiControlPresentation::Numeric { value: self.progress_val, min: 0.0, max: 1.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-prog-striped".into()),
                state: UiControlPresentation::Numeric { value: self.progress_val, min: 0.0, max: 1.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-prog-rounded".into()),
                state: UiControlPresentation::Numeric { value: self.progress_val, min: 0.0, max: 1.0 },
            },
            // Scrollbar variants
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-scroll-def".into()),
                state: UiControlPresentation::Scroll { position: self.scroll_pos },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-scroll-fat".into()),
                state: UiControlPresentation::Scroll { position: self.scroll_pos },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-scroll-dark".into()),
                state: UiControlPresentation::Scroll { position: self.scroll_pos },
            },
            // Radio variants
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-radio-def".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_radio_def },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-radio-circle".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_radio_circle },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("skin-radio-card".into()),
                state: UiControlPresentation::Toggle { selected: self.skin_radio_card },
            },
        ]
    }

    /// Apply visibility and text changes to the root node.
    fn apply(&self, node: &mut UiNode) {
        self.tree.apply(node);
        // Context menu visibility
        if node.node_id.0 == "demo-context" {
            node.visible = self.context_menu_visible;
        }
        // Button text shows click count
        if node.node_id.0 == "btn-demo" && self.click_count > 0 {
            if let Some(text) = &mut node.text {
                *text = TextRef::Literal {
                    value: format!("Clicked {} times", self.click_count),
                };
            }
        }
        for child in &mut node.children {
            self.apply(child);
        }
    }

    fn handle_action(&mut self, action: &str) {
        match action {
            "demo.button.click" => {
                self.click_count += 1;
                self.progress_val = (self.progress_val + 0.05).min(1.0);
                println!("[button] clicked {} times, progress={:.2}", self.click_count, self.progress_val);
            }
            "demo.check.toggle" => {
                self.checkbox_state = !self.checkbox_state;
                println!("[checkbox] -> {}", self.checkbox_state);
            }
            "demo.skin.check.def" => { self.skin_check_def = !self.skin_check_def; }
            "demo.skin.check.circle" => { self.skin_check_circle = !self.skin_check_circle; }
            "demo.skin.check.card" => { self.skin_check_card = !self.skin_check_card; }
            "demo.skin.radio.def" => { self.skin_radio_def = !self.skin_radio_def; }
            "demo.skin.radio.circle" => { self.skin_radio_circle = !self.skin_radio_circle; }
            "demo.skin.radio.card" => { self.skin_radio_card = !self.skin_radio_card; }
            "demo.radio.toggle" => {
                self.radio_state = !self.radio_state;
                println!("[radio] -> {}", self.radio_state);
            }
            "demo.switch.toggle" => {
                self.switch_state = !self.switch_state;
                println!("[switch] -> {}", self.switch_state);
            }
            "demo.scroll.up" => {
                self.scroll_pos = (self.scroll_pos - 0.1).max(0.0);
                println!("[scroll] up -> {:.2}", self.scroll_pos);
            }
            "demo.scroll.down" => {
                self.scroll_pos = (self.scroll_pos + 0.1).min(1.0);
                println!("[scroll] down -> {:.2}", self.scroll_pos);
            }
            "demo.ctx.show" => {
                self.context_menu_visible = !self.context_menu_visible;
                println!("[ctx] menu visible={}", self.context_menu_visible);
            }
            "demo.ctx.copy" | "demo.ctx.paste" | "demo.ctx.delete" => {
                println!("[ctx] item: {action}");
                self.context_menu_visible = false;
            }
            "demo.splitter.toggle" => {
                self.splitter_mode = (self.splitter_mode + 1) % 3;
                let labels = ["50/50", "30/70", "70/30"];
                println!("[splitter] mode={}", labels[self.splitter_mode as usize]);
            }
            "demo.tree.root" => { self.tree.toggle_project(); println!("[tree] project={}", self.tree.project_expanded); }
            "demo.tree.crates" => { self.tree.toggle_crates(); println!("[tree] crates={}", self.tree.crates_expanded); }
            "demo.tree.docs" | "demo.tree.cases" | "demo.tree.schema" | "demo.tree.runtime" | "demo.tree.wgpu" => {
                println!("[tree] leaf: {action}");
            }
            _ => println!("[unknown] {action}"),
        }
    }
}

/// Starts a minimal UI-host RPC server that receives semantic events.
fn start_ui_host_server(click_queue: Arc<Mutex<Vec<String>>>) {
    let endpoint: SocketAddr = UI_ENDPOINT.parse().unwrap();
    let server = match RpcServer::bind(endpoint) {
        Ok(server) => server,
        Err(error) => { eprintln!("[ui-host] bind failed: {error}"); return; }
    };
    println!("[ui-host] listening on {UI_ENDPOINT}");
    thread::spawn(move || {
        let _ = server.serve_until(move |req: RpcRequest| {
            if req.method == "service.shutdown" {
                return (RpcResponse {
                    request_id: req.request_id.clone(), status: RpcStatus::Accepted,
                    revision: None, result: Some(json!({"state":"accepted"})),
                    error: None, snapshot: None,
                }, false);
            }
            if req.method == "ui.host.inbound" {
                if let Some(action) = serde_json::from_value::<UiSemanticEvent>(req.params.clone())
                    .ok()
                    .and_then(|event| {
                        let neon_ui_schema::UiIntent::Invoke { action, .. } = event.intent;
                        Some(action)
                    })
                {
                    println!("[ui-host] click: {action}");
                    if let Ok(mut q) = click_queue.lock() { q.push(action); }
                }
            }
            (RpcResponse {
                request_id: req.request_id.clone(), status: RpcStatus::Accepted,
                revision: None, result: Some(json!({"state":"accepted"})),
                error: None, snapshot: None,
            }, true)
        });
    });
}

fn main() -> Result<(), String> {
    println!("=== Component Showcase Probe (interactive) ===");
    let (root, base_effects) = load_nui_fragment()?;
    println!("Parsed: root={}, effects={}", root.node_id.0, base_effects.len());

    let click_queue = Arc::new(Mutex::new(Vec::<String>::new()));
    start_ui_host_server(click_queue.clone());

    let mut child = launch().map_err(|e| format!("launch failed: {e}"))?;
    thread::sleep(Duration::from_secs(3));
    let endpoint: SocketAddr = ENDPOINT.parse().unwrap();

    // Upload skin images before submitting the fragment.
    upload_showcase_images(endpoint)?;

    let mut root = root;
    root.surface = None;

    let mut state = AppState::new();
    let mut revision = 1u64;

    // Initial submit
    let mut display_root = root.clone();
    state.apply(&mut display_root);
    let mut effects = base_effects.clone();
    effects.extend(state.presentation_effects());
    let fragment = UiFragment {
        fragment_id: UiFragmentId("showcase".into()),
        revision: Revision(revision),
        root: display_root,
        effects,
    };
    call(endpoint, "wgpu.ui.submit_fragment", revision,
        serde_json::to_value(&UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }).unwrap())?;
    println!("Initial fragment submitted. Click around!");

    loop {
        thread::sleep(Duration::from_millis(200));
        if child.try_wait().ok().flatten().is_some() {
            println!("Runtime exited"); break;
        }

        let clicks: Vec<String> = {
            let mut q = click_queue.lock().unwrap();
            std::mem::take(&mut *q)
        };
        let mut changed = !clicks.is_empty();
        for action in &clicks {
            state.handle_action(action);
        }

        revision += 1;
        let mut display_root = root.clone();
        state.apply(&mut display_root);
        let mut effects = base_effects.clone();
        effects.extend(state.presentation_effects());
        let fragment = UiFragment {
            fragment_id: UiFragmentId("showcase".into()),
            revision: Revision(revision),
            root: display_root,
            effects,
        };
        if let Err(e) = call(endpoint, "wgpu.ui.submit_fragment", revision,
            serde_json::to_value(&UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }).unwrap())
        {
            eprintln!("heartbeat failed: {e}");
        }
        let _ = changed;
    }

    let _ = child.kill();
    Ok(())
}
