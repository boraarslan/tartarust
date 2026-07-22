//! Thin shim: the real CLI lives in [`tartarust_cli`]. This bin target exists in
//! the root package so one `cargo install --path .` ships it (named `tartarust`).
fn main() -> anyhow::Result<()> {
    tartarust_cli::main()
}
