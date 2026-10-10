#![allow(dead_code, unused_imports)]

#[cfg(target_os = "macos")]
#[path = "../src/focused_app.rs"]
mod focused_app;
#[cfg(target_os = "macos")]
#[path = "../src/insertion.rs"]
mod insertion;

#[cfg(target_os = "macos")]
fn main() {
    insertion::run_macos_named_pasteboard_adapter_checks();
    println!("macOS named-pasteboard main-thread checks passed");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macOS named-pasteboard main-thread checks skipped on non-macOS");
}
