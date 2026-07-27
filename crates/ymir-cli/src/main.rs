//! Temporary runner: build a three-node graph (fBm generator -> thermal erosion ->
//! PNG export endpoint), save it as a project file, then reload that file and render
//! from the reloaded graph, so `cargo run` exercises the full save/load path end to
//! end and leaves an inspectable `project.json`. This will grow into a real
//! graph-driven CLI.

use std::error::Error;

use ymir_core::registry::make;
use ymir_core::{EvalCache, EvalRequest, Graph, ParamValue, Params, Region};

// Anchor ymir-nodes so its operator registrations link into this binary. Without
// this the binary only references ymir-core (the registry), nothing names
// ymir-nodes, and the linker can drop its registrations entirely.
use ymir_nodes as _;

mod docs;
mod render;

fn make_op(type_id: &str) -> Result<Box<dyn ymir_core::Operator>, Box<dyn Error>> {
    make(type_id).ok_or_else(|| format!("operator {type_id:?} is not registered").into())
}

/// What the command line asked for. Parsed apart from `main` so the dispatch is unit-tested
/// rather than exercised only by running the binary.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// No arguments: render the built-in sample graph.
    Sample,
    /// `render …`, with the arguments after it passed through.
    Render(Vec<String>),
    /// `docs …`, with the arguments after it passed through.
    Docs(Vec<String>),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h`.
    Help,
    /// Something unrecognised, which is a mistake rather than a request.
    Unknown(String),
}

/// Maps the arguments after the binary name onto a [`Command`].
///
/// An unrecognised argument is [`Command::Unknown`], never a silent fall-through to the sample
/// render (#276). Rendering on a typo writes files, prints success and exits zero, so a mistyped
/// subcommand was indistinguishable from asking for the sample, including to a script checking
/// the exit status.
fn parse(args: &[String]) -> Command {
    // Version and help win wherever they appear: they are questions about the binary, and a user
    // who types one alongside anything else wants the answer, not the work.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Command::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Command::Help;
    }
    match args.split_first() {
        None => Command::Sample,
        Some((first, rest)) if first == "render" => Command::Render(rest.to_vec()),
        Some((first, rest)) if first == "docs" => Command::Docs(rest.to_vec()),
        Some((first, _)) => Command::Unknown(first.clone()),
    }
}

/// What the CLI can do, in the order it is likely to be wanted.
const USAGE: &str = "\
ymir-cli, the headless runner for Ymir.

Usage:
  ymir-cli render PROJECT [--res N] [--out FILE] [--node NAME]
                               Build a saved project. The seed, world size, and build
                               resolution come from the project itself, so the result
                               matches what the editor shows.

                               --res N     Build at N cells square instead of the
                                           project's own build resolution.
                               --out FILE  Write the result here, as .png, .r16, or
                                           .exr. Required when the project ends in an
                                           ordinary node; refused when it ends in
                                           export nodes, which write their own paths.
                               --node NAME Build one node, by its editor name or its
                                           stable id, instead of every result node.

  ymir-cli                     Render the built-in sample graph to out/, exercising the
                               full save and reload path.
  ymir-cli docs [--format json]
                               Print the node reference as JSON, generated from this
                               binary's own registry.
  ymir-cli --version, -V       Print the version, with the commit it was built from.
  ymir-cli --help, -h          Print this.";

/// Runs the command and reports a failure as a plain sentence.
///
/// `main` does not return the `Result` itself: Rust's own reporting prints it through `Debug`, so
/// a message that reads "no node named X in this project" arrives quoted and backslash-escaped.
/// What someone typing a command wants is the sentence.
fn main() {
    if let Err(err) = run() {
        eprintln!("ymir-cli: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        // Printed before any work, so it stays usable for provenance even if a render would fail.
        Command::Version => {
            println!("ymir {}", ymir_build_info::version_string());
            return Ok(());
        }
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        // Emitted before any logging or render work.
        Command::Docs(rest) => return docs::run(&rest),
        Command::Render(rest) => {
            // Load degradations go to stderr rather than being swallowed, the same as the sample.
            ymir_core::logging::init(None, log::LevelFilter::Info);
            return render::run(&rest);
        }
        Command::Unknown(arg) => {
            eprintln!("ymir-cli: unrecognised argument {arg:?}\n\n{USAGE}");
            std::process::exit(2);
        }
        Command::Sample => {}
    }

    // Headless diagnostics go to stderr (a toolchain captures it); load degradations are logged
    // rather than swallowed.
    ymir_core::logging::init(None, log::LevelFilter::Info);

    let size: usize = 512;
    let seed: u64 = 42;
    let path = "out/heightmap.png";
    let project_path = "out/project.json";

    let mut graph = Graph::new();
    let generator = graph.add_op(make_op("generator.fbm")?, Params::default());
    let erosion = graph.add_op(make_op("modifier.thermal_erosion")?, Params::default());
    let export = graph.add_op(
        make_op("endpoint.export")?,
        Params::new().with("path", ParamValue::Text(path.to_string())),
    );

    graph.connect(generator, 0, erosion, 0)?;
    graph.connect(erosion, 0, export, 0)?;

    // Save the project, then reload it and render from the reloaded graph, so the run
    // proves the full save/load round-trip rather than just evaluating in memory.
    std::fs::create_dir_all("out")?;
    graph.save(project_path)?;
    let export_id = graph
        .stable_id(export)
        .ok_or("export node has no stable id")?;
    let (graph, warnings) = Graph::load_reporting(project_path)?;
    for warning in &warnings {
        log::warn!("loading {project_path}: {warning}");
    }
    let export = graph
        .node_id_of(export_id)
        .ok_or("export node missing after reload")?;

    // Pulling the endpoint evaluates the chain and writes the file as a side
    // effect (endpoints are not memoized). Run erosion on the GPU when a device is reachable,
    // else on the CPU: a headless host with no adapter falls back cleanly.
    let mut request = EvalRequest::new(size, size, Region::UNIT, seed);
    match ymir_gpu::GpuContext::new_headless() {
        Ok(gpu) => request = request.with_compute(std::sync::Arc::new(gpu)),
        Err(err) => log::info!("no GPU device, rendering on CPU: {err}"),
    }
    let mut cache = EvalCache::new(64);
    graph.evaluate(export, &request, &mut cache)?;

    println!("saved project to {project_path}");
    println!(
        "wrote {path} ({size}x{size}, 16-bit grayscale, fBm + thermal erosion, seed {seed}) from the reloaded project"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, parse};
    use ymir_core::registry;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_renders_the_sample() {
        assert_eq!(parse(&args(&[])), Command::Sample);
    }

    #[test]
    fn an_unrecognised_argument_is_not_a_request_to_render() {
        // #276: falling through to the sample meant a typo wrote files, printed success and
        // exited zero, so a mistyped subcommand was indistinguishable from asking for a render.
        assert_eq!(
            parse(&args(&["dcos"])),
            Command::Unknown("dcos".to_string())
        );
        assert_eq!(
            parse(&args(&["--hepl"])),
            Command::Unknown("--hepl".to_string())
        );
        assert_eq!(
            parse(&args(&["renderr", "project.ymir"])),
            Command::Unknown("renderr".to_string()),
            "a near-miss on a real command is refused, not silently ignored"
        );
    }

    #[test]
    fn render_passes_the_rest_through() {
        // The subcommand parses its own arguments (see `render::parse`), so this only has to
        // route them there intact.
        assert_eq!(
            parse(&args(&["render", "p.ymir", "--res", "512"])),
            Command::Render(args(&["p.ymir", "--res", "512"]))
        );
    }

    #[test]
    fn docs_passes_the_rest_through() {
        assert_eq!(parse(&args(&["docs"])), Command::Docs(Vec::new()));
        assert_eq!(
            parse(&args(&["docs", "--format", "json"])),
            Command::Docs(args(&["--format", "json"]))
        );
    }

    #[test]
    fn version_and_help_win_wherever_they_appear() {
        // They are questions about the binary, so someone who types one alongside anything else
        // wants the answer rather than the work.
        for form in ["--version", "-V"] {
            assert_eq!(parse(&args(&[form])), Command::Version);
            assert_eq!(parse(&args(&["docs", form])), Command::Version);
        }
        for form in ["--help", "-h"] {
            assert_eq!(parse(&args(&[form])), Command::Help);
            assert_eq!(parse(&args(&["docs", form])), Command::Help);
        }
    }

    // Link-anchor smoke test: proves the `use ymir_nodes as _` above actually pulls
    // ymir-nodes' operator registrations into *this binary*. Without the anchor the
    // linker can drop them (the inventory gotcha) and the registry comes up empty, so
    // asserting a couple of sentinel operators construct fails fast here. The full
    // registered set is pinned once in crates/ymir-nodes/tests/registry_smoke.rs; this
    // stays a per-binary link check and deliberately does not re-list every node.
    #[test]
    fn ymir_nodes_is_linked_into_this_binary() {
        assert!(
            registry::count() > 0,
            "operator registry is empty; ymir-nodes was not linked",
        );
        for type_id in [
            "generator.fbm",
            "modifier.thermal_erosion",
            "endpoint.export",
        ] {
            assert!(
                registry::make(type_id).is_some(),
                "operator {type_id:?} is not registered; the ymir-nodes anchor was dropped",
            );
        }
    }
}
