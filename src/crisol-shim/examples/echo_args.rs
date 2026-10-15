//! A stand-in for `php.exe` / `node.exe` in the shim tests: it prints what it received, so
//! the tests can check that the shim passed everything on faithfully. Not part of the product.
//!
//! Output, one item per line: `EXE:` (its own path, which says which installation ran),
//! `CWD:`, `PHPRC:` and `CRISOL_PHPRC:` (or `-`), one `ARG:<n>:` line per argument, and `STDIN:` with what it
//! read. The exit code is taken from `ECHO_EXIT` (0 if unset).

use std::io::Read;

fn main() {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    println!("EXE:{exe}");
    println!(
        "CWD:{}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    println!(
        "PHPRC:{}",
        std::env::var("PHPRC").unwrap_or_else(|_| "-".to_owned())
    );
    println!(
        "CRISOL_PHPRC:{}",
        std::env::var("CRISOL_PHPRC").unwrap_or_else(|_| "-".to_owned())
    );
    for (index, arg) in std::env::args_os().skip(1).enumerate() {
        println!("ARG:{index}:{}", arg.to_string_lossy());
    }
    if std::env::var_os("ECHO_STDIN").is_some() {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        println!("STDIN:{}", input.replace('\n', "\\n"));
    }
    let code = std::env::var("ECHO_EXIT")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
