use clap::Parser;

/// Crisol command line interface.
#[derive(Parser)]
#[command(name = "crisol", version)]
struct Args {}

fn main() {
    let _args = Args::parse();
}
