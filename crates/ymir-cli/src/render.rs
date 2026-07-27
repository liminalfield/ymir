//! `ymir-cli render`: build a saved project headlessly (#30).
//!
//! The point of the project file carrying its world settings (#309) is that this can reproduce
//! what the editor shows. Seed, world extent, world height, sea level, and the build resolution
//! all come out of the file, through [`EvalRequest::from_world`], which is the same constructor
//! the editor uses. Nothing here invents a world.
//!
//! What gets built is the graph's sinks, which is what a Build means in the editor too. A project
//! that ends in export endpoints writes the files those endpoints name. A project that ends in a
//! modifier, which is what an editing session usually leaves behind, has nowhere to write, so
//! `--out` names the file and this module encodes it.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ymir_core::export::{HeightRange, export_exr, export_png, export_r16};
use ymir_core::{EvalCache, EvalRequest, Field, Graph, NodeId, ProjectFile};

/// How many nodes' results the cache keeps. A headless render walks each sink once, so this only
/// has to span the branches of a graph that reconverge, not a whole editing session.
const CACHE_NODES: usize = 64;

/// What `render` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// The project file to build.
    project: PathBuf,
    /// Square resolution to build at. `None` takes the project's own `build_res`, which is the
    /// resolution the project was authored to produce.
    res: Option<usize>,
    /// Where to write. Required when the target is not an export endpoint, refused when it is.
    out: Option<PathBuf>,
    /// Which node to build, by display name or stable id. `None` builds every sink.
    node: Option<String>,
}

/// Parses the arguments after `render`.
///
/// Every failure is a message rather than a partial parse: a render writes files, so acting on a
/// half-understood command line is worse than refusing it.
pub(crate) fn parse(args: &[String]) -> Result<Args, String> {
    let mut project: Option<PathBuf> = None;
    let mut res: Option<usize> = None;
    let mut out: Option<PathBuf> = None;
    let mut node: Option<String> = None;

    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        // A flag's value is taken here rather than after the loop, so `--res` with nothing after
        // it is an error instead of silently taking the next flag as its value.
        let mut value = |flag: &str| -> Result<String, String> {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--res" => {
                let raw = value("--res")?;
                let parsed: usize = raw
                    .parse()
                    .map_err(|_| format!("--res wants a number of cells, not {raw:?}"))?;
                if parsed == 0 {
                    return Err("--res must be at least 1".to_string());
                }
                res = Some(parsed);
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--node" => node = Some(value("--node")?),
            other if other.starts_with('-') => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other if project.is_none() => project = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    Ok(Args {
        project: project.ok_or("name a project file to render")?,
        res,
        out,
        node,
    })
}

/// The image format to write, chosen by the `--out` extension.
///
/// From the extension rather than a flag because the extension already states the intent, and a
/// file named `.png` holding EXR data is a worse outcome than being told the extension is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    R16,
    Exr,
}

impl Format {
    /// The format `path` names, or an error listing what is understood.
    fn of(path: &Path) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("png") => Ok(Self::Png),
            Some("r16") => Ok(Self::R16),
            Some("exr") => Ok(Self::Exr),
            Some(other) => Err(format!(
                "cannot write {other:?}; --out understands .png, .r16, and .exr"
            )),
            None => Err(format!(
                "{} has no extension, so there is nothing to say what format to write",
                path.display()
            )),
        }
    }

    /// Writes `field`'s height layer. `world_height` scales EXR output to metres, matching what
    /// the Export EXR node writes in its Meters mode, so a file from either path means the same.
    fn write(self, field: &Field, path: &Path, world_height: f64) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        match self {
            // Auto-range, matching the export endpoints' own default: terrain that ran outside
            // [0, 1] upstream is preserved rather than clipped.
            Self::Png => export_png(field, path, HeightRange::Auto)?,
            Self::R16 => export_r16(field, path, HeightRange::Auto)?,
            Self::Exr => export_exr(field, path, world_height as f32)?,
        }
        Ok(())
    }
}

/// Resolves `--node` against the graph: a display name, case-insensitively, else a stable id.
///
/// Names first because that is what the editor shows. A name shared by several nodes is an error
/// listing their stable ids rather than a guess, since rendering the wrong branch of a graph
/// produces a plausible-looking file that is simply not what was asked for.
fn find_node(graph: &Graph, wanted: &str) -> Result<NodeId, String> {
    let named: Vec<NodeId> = graph
        .node_ids()
        .into_iter()
        .filter(|&id| {
            graph
                .name(id)
                .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
        })
        .collect();
    match named.as_slice() {
        [only] => return Ok(*only),
        [] => {}
        several => {
            let ids: Vec<String> = several
                .iter()
                .filter_map(|&id| graph.stable_id(id))
                .map(|s| s.to_string())
                .collect();
            return Err(format!(
                "{} nodes are named {wanted:?} (stable ids {}); name one by its stable id",
                several.len(),
                ids.join(", ")
            ));
        }
    }

    // No name matched, so a bare number is a stable id. Checked second: a node the user renamed
    // to "12" should resolve to that node, not to stable id 12.
    if let Ok(stable_id) = wanted.parse::<u64>()
        && let Some(id) = graph.node_id_of(stable_id)
    {
        return Ok(id);
    }
    // Say what would work. A project whose nodes were never renamed has nothing to match by
    // name at all, and being told only that the name was not found leaves no next move.
    let names: Vec<&str> = graph
        .node_ids()
        .into_iter()
        .filter_map(|id| graph.name(id))
        .collect();
    if names.is_empty() {
        let ids: Vec<String> = graph
            .node_ids()
            .into_iter()
            .filter_map(|id| graph.stable_id(id))
            .map(|s| s.to_string())
            .collect();
        return Err(format!(
            "no node named {wanted:?}; nothing in this project is named, so use a stable id ({})",
            ids.join(", ")
        ));
    }
    Err(format!(
        "no node named {wanted:?}; this project has {}",
        names
            .iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A one-line description of a node for a message: its display name if it has one, else its type.
fn describe(graph: &Graph, id: NodeId) -> String {
    let stable_id = graph.stable_id(id).unwrap_or_default();
    match graph.name(id) {
        Some(name) => format!("{name} (stable id {stable_id})"),
        None => {
            let type_id = graph
                .spec(id)
                .map_or_else(String::new, |s| s.type_id.into());
            format!("{type_id} (stable id {stable_id})")
        }
    }
}

/// Whether a node writes its own file: an endpoint, by arity.
fn is_endpoint(graph: &Graph, id: NodeId) -> bool {
    graph.spec(id).is_some_and(|s| s.outputs.is_empty())
}

/// Whether an endpoint is included in a build, from its own `build` parameter. A node without the
/// parameter is included, so a future endpoint type needs no change here.
fn included_in_build(graph: &Graph, id: NodeId) -> bool {
    graph.params(id).is_none_or(|p| p.get_bool("build", true))
}

/// Runs `render`.
pub(crate) fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let args = parse(args)?;
    // The path is folded into the message: "No such file or directory" alone leaves the user
    // guessing which of the paths they typed was wrong.
    let file: ProjectFile = ProjectFile::load(&args.project)
        .map_err(|e| format!("cannot open {}: {e}", args.project.display()))?;
    // Checked explicitly, so an older project says what it is rather than failing somewhere
    // downstream on a graph that never loaded properly.
    file.check_version()
        .map_err(|e| format!("{}: {e}", args.project.display()))?;

    let (graph, warnings) = Graph::from_document_reporting(&file.graph)?;
    for warning in &warnings {
        log::warn!("{}: {warning}", args.project.display());
    }

    let targets = match &args.node {
        Some(wanted) => vec![find_node(&graph, wanted)?],
        None => {
            let sinks = graph.sinks();
            if sinks.is_empty() {
                return Err(format!("{} has no nodes to build", args.project.display()).into());
            }
            // An endpoint switched off in the editor stays off here: it is the project's own
            // statement about what a build produces.
            let (endpoints, others): (Vec<NodeId>, Vec<NodeId>) =
                sinks.into_iter().partition(|&id| is_endpoint(&graph, id));
            let active: Vec<NodeId> = endpoints
                .into_iter()
                .filter(|&id| included_in_build(&graph, id))
                .collect();
            let targets: Vec<NodeId> = active.into_iter().chain(others).collect();
            if targets.is_empty() {
                return Err(format!(
                    "every output in {} is switched off for builds",
                    args.project.display()
                )
                .into());
            }
            targets
        }
    };

    // An endpoint writes the path it carries, so --out has nothing to name; a node with outputs
    // has no path of its own, so --out is the only way to get anything on disk. Refusing the
    // first case rather than quietly ignoring --out keeps a file from being written somewhere
    // other than where the flag said.
    let writing: Vec<NodeId> = targets
        .iter()
        .copied()
        .filter(|&id| !is_endpoint(&graph, id))
        .collect();
    match (&args.out, writing.as_slice()) {
        (Some(_), []) => {
            return Err(match &args.node {
                Some(named) => format!(
                    "{named:?} is an export node, which writes the path it carries; drop --out, \
                     or name a node that produces a field"
                ),
                None => "this project's outputs write the paths they carry, so --out would be \
                         ambiguous; drop it, or name a node with --node"
                    .to_string(),
            }
            .into());
        }
        (Some(_), [_only]) => {}
        (Some(_), several) => {
            let names: Vec<String> = several.iter().map(|&id| describe(&graph, id)).collect();
            return Err(format!(
                "--out names one file, but this project has {} nodes to build ({}); \
                 pick one with --node",
                several.len(),
                names.join(", ")
            )
            .into());
        }
        (None, []) => {}
        (None, several) => {
            let names: Vec<String> = several.iter().map(|&id| describe(&graph, id)).collect();
            return Err(format!(
                "nothing to write: {} produces a field but is not an export node, so name a \
                 file with --out",
                names.join(", ")
            )
            .into());
        }
    }
    // Rejected before the render rather than after it, so an unwritable extension costs nothing.
    let format = args.out.as_deref().map(Format::of).transpose()?;

    let res = args.res.unwrap_or(file.world.build_res);
    let mut request = EvalRequest::from_world(&file.world, res);
    // Erosion runs on the GPU when a device is reachable. A host with none (CI, a machine with no
    // capable adapter) falls back to the CPU path, which is the reference implementation.
    match ymir_gpu::GpuContext::new_headless() {
        Ok(gpu) => request = request.with_compute(Arc::new(gpu)),
        Err(err) => log::info!("no GPU device, rendering on CPU: {err}"),
    }

    let mut cache = EvalCache::new(CACHE_NODES);
    for &id in &targets {
        let outputs = graph.evaluate(id, &request, &mut cache)?;
        if let (Some(path), Some(format)) = (&args.out, format) {
            let field = outputs
                .first()
                .ok_or_else(|| format!("{} produced no output to write", describe(&graph, id)))?;
            format.write(field, path, file.world.world_height)?;
            println!("wrote {}", path.display());
        }
    }

    // Endpoints wrote their own paths as a side effect of being pulled, and only they know where.
    // Reporting the count rather than inventing a list keeps this honest.
    if args.out.is_none() {
        println!(
            "built {} output{} at {res}x{res}",
            targets.len(),
            if targets.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_project_path_is_enough() {
        assert_eq!(
            parse(&args(&["project.ymir"])),
            Ok(Args {
                project: PathBuf::from("project.ymir"),
                res: None,
                out: None,
                node: None,
            })
        );
    }

    #[test]
    fn flags_parse_in_any_order() {
        let expected = Args {
            project: PathBuf::from("p.ymir"),
            res: Some(1024),
            out: Some(PathBuf::from("out.png")),
            node: Some("Coastal".to_string()),
        };
        assert_eq!(
            parse(&args(&[
                "p.ymir", "--res", "1024", "--out", "out.png", "--node", "Coastal"
            ])),
            Ok(expected.clone())
        );
        assert_eq!(
            parse(&args(&[
                "--node", "Coastal", "--out", "out.png", "--res", "1024", "p.ymir"
            ])),
            Ok(expected)
        );
    }

    #[test]
    fn a_flag_with_no_value_is_refused_not_fed_the_next_flag() {
        // `--res --out x.png` must not parse `--out` as the resolution and then lose the output
        // path entirely, which would render at a default size and write nothing.
        assert!(parse(&args(&["p.ymir", "--res"])).is_err());
        assert!(parse(&args(&["p.ymir", "--out"])).is_err());
        assert_eq!(
            parse(&args(&["p.ymir", "--res", "--out", "x.png"])),
            Err("--res wants a number of cells, not \"--out\"".to_string())
        );
    }

    #[test]
    fn a_render_needs_a_project_and_a_usable_resolution() {
        assert_eq!(
            parse(&args(&[])),
            Err("name a project file to render".to_string())
        );
        assert!(parse(&args(&["p.ymir", "--res", "0"])).is_err());
        assert!(parse(&args(&["p.ymir", "--res", "-1"])).is_err());
        assert!(parse(&args(&["p.ymir", "--wat"])).is_err());
        assert!(
            parse(&args(&["a.ymir", "b.ymir"])).is_err(),
            "two projects is a mistake, not a batch"
        );
    }

    #[test]
    fn the_output_format_comes_from_the_extension() {
        assert_eq!(Format::of(Path::new("a.png")), Ok(Format::Png));
        assert_eq!(Format::of(Path::new("a.R16")), Ok(Format::R16));
        assert_eq!(Format::of(Path::new("out/deep/a.exr")), Ok(Format::Exr));
        assert!(Format::of(Path::new("a.tiff")).is_err());
        assert!(
            Format::of(Path::new("heightmap")).is_err(),
            "a bare name says nothing about the format, so guessing one would be worse"
        );
    }
}
