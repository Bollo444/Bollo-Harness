//! Hook fixture binary used only by integration tests.
//!
//! Reads all of stdin (so the parent's write cannot block) and then behaves
//! according to the scenario argument: continue, block, invalid, exit3, sleep
//! or echo-stdin.

use std::io::Read;

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_default();
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    match scenario.as_str() {
        "continue" => println!("{{\"decision\":\"continue\"}}"),
        "block" => println!("{{\"decision\":\"block\",\"reason\":\"fixture block\"}}"),
        "invalid" => println!("not-json"),
        "empty" => {}
        "exit3" => std::process::exit(3),
        "sleep" => std::thread::sleep(std::time::Duration::from_secs(5)),
        "echo-stdin" => println!(
            "{{\"decision\":\"continue\",\"reason\":\"stdin-len:{}\"}}",
            input.len()
        ),
        _ => std::process::exit(2),
    }
}
