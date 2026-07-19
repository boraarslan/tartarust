//! Thin shim: the real CLI lives in [`tartarus_cli`]. This bin target exists in
//! the root package so one `cargo install --path .` ships it (named `tartarus`).
fn main() -> anyhow::Result<()> {
    tartarus_cli::main()
}
