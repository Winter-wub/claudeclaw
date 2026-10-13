//! Repo tasks, run as `cargo xtask <task>`.

mod deps;

use std::process::ExitCode;

use anyhow::Result;

const USAGE: &str = "usage: cargo xtask <task>

tasks:
  deps    enforce the workspace dependency rules (architecture §3.1)";

fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["deps"] => deps::run(),
        ["-h" | "--help"] => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::from(2))
        }
    }
}
