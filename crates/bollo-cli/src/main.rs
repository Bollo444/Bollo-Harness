//! `bollo` entry point. All logic lives in the library so integration tests can
//! drive the same code paths the binary uses.

fn main() {
    let code = bollo_cli::run_from(std::env::args());
    std::process::exit(code);
}
