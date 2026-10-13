use clap::Parser;

/// Crisol daemon: keeps services alive and serves the IPC.
#[derive(Parser)]
#[command(name = "crisold", version)]
struct Args {}

fn main() {
    let _args = Args::parse();
}
