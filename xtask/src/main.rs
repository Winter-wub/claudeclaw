//! Repo tasks, run as `cargo xtask <task>`.

mod deps;
mod openapi;

use std::process::ExitCode;

use anyhow::Result;

const USAGE: &str = "usage: cargo xtask <task>

tasks:
  deps               enforce the workspace dependency rules (architecture §3.1)
  openapi [--check]  write crates/api/openapi.json, or fail if it is stale";

fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["deps"] => deps::run(),
        ["openapi"] => openapi::run(false),
        ["openapi", "--check"] => openapi::run(true),
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
