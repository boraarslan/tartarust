//! Thin shim: the real app lives in [`tartarust_ui`]. This bin target exists in
//! the root package so one `cargo install --path .` ships it.
//!
//! Dev rule still applies: NEVER run this bare — only inside the headless cage
//! harness (`scripts/headless-screenshot.sh`).
fn main() {
    tartarust_ui::run_app()
}
