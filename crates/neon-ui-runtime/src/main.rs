//! Headless UI declaration runtime. It must not create windows or GPU objects.

fn main() {
    let _runtime = neon_ui_runtime::UiRuntime::new(1, "ui-runtime-local");
}
