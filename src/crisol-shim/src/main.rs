//! Tiny launcher: must start in milliseconds, so no heavy dependencies.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("crisol-shim {}", env!("CARGO_PKG_VERSION"));
    }
}
