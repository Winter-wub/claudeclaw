//! `cargo xtask openapi [--check]`: writes `crates/api/openapi.json` from `claudeclaw_api`, or
//! fails if the committed file is stale (architecture §4).

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

const FILE: &str = "crates/api/openapi.json";

pub fn run(check: bool) -> Result<ExitCode> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask has no parent dir")?;
    let path = root.join(FILE);
    let generated = render()?;
    if !check {
        fs::write(&path, &generated).with_context(|| format!("writing {FILE}"))?;
        println!("openapi: wrote {FILE}");
        return Ok(ExitCode::SUCCESS);
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    if committed == generated {
        println!("openapi: {FILE} is current");
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("openapi: {FILE} is stale. Run `cargo xtask openapi` and commit the result.");
    Ok(ExitCode::FAILURE)
}

fn render() -> Result<String> {
    let json = claudeclaw_api::openapi()
        .to_pretty_json()
        .context("serialising the OpenAPI document")?;
    Ok(json + "\n")
}
