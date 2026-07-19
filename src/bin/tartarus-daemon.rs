//! Thin shim: the real daemon CLI lives in [`tartarus_daemon::cli`]. This bin
//! target exists in the root package so one `cargo install --path .` ships it.
fn main() -> anyhow::Result<()> {
    tartarus_daemon::cli::main()
}
