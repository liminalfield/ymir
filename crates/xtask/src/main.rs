//! Developer tasks for Ymir, run via `cargo xtask <task>`.
//!
//! Kept out of the shipped binaries: this crate holds tooling for the maintainer and CI, not
//! runtime code. The first task is `docs-gen`, which turns the node reference JSON that
//! `ymir docs --format json` emits into the Markdown pages under `docs/reference/`.

use std::process::ExitCode;

mod docs;
mod fragment;
mod lint;
mod render;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("docs-gen") => docs::run(&args[1..]),
        Some("docs-lint") => lint::run(&args[1..]),
        Some(other) => Err(format!("unknown task {other:?} (tasks: docs-gen, docs-lint)").into()),
        None => Err("usage: cargo xtask <task> (tasks: docs-gen, docs-lint)".into()),
    }
}
