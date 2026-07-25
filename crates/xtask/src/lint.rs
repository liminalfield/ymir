//! `cargo xtask docs-lint`: checks the documentation for the mechanical faults a human reviewer
//! should not have to catch.
//!
//! Findings have two severities. An **error** fails the lint (and CI): an orphaned fragment, an
//! invalid fragment, a broken intra-site link, a missing figure, a forbidden marker in a published
//! page, a relative link into `design/`, or a completeness violation on a page marked `stable` (an
//! empty Purpose, or a parameter that fell through to the prettified fallback). A **gap** is
//! reported but not fatal: a node with no prose fragment yet, which is expected while the reference
//! is still being written. What the lint cannot judge (whether a claim is true, whether prose reads
//! well) stays with human review, per `DOCS.md`.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::docs::{self, Docs};
use crate::fragment::{self, Fragment};

/// Case-insensitive substrings that must not appear in a published page: leftover task markers and
/// the design-record vocabulary that reads as an unfinished product in a manual.
const FORBIDDEN_MARKERS: [&str; 4] = ["todo", "tbd", "open question", "proposed"];

/// A lint finding and whether it fails the run.
struct Finding {
    severity: Severity,
    message: String,
}

#[derive(PartialEq, Eq)]
enum Severity {
    Error,
    Gap,
}

impl Finding {
    fn error(message: String) -> Self {
        Self {
            severity: Severity::Error,
            message,
        }
    }

    fn gap(message: String) -> Self {
        Self {
            severity: Severity::Gap,
            message,
        }
    }
}

/// Runs `docs-lint`. Accepts an optional `--json <path>` (as `docs-gen` does) so CI can lint against
/// a captured document without shelling out again.
pub(crate) fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut json_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json_path = Some(PathBuf::from(args.get(i + 1).ok_or("--json needs a path")?));
                i += 2;
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }

    let docs = docs::load(json_path.as_deref())?;
    let root = docs::workspace_root();

    let mut findings = Vec::new();
    check_fragments(&docs, &root, &mut findings)?;
    check_published_pages(&root, &mut findings)?;
    report(&findings);

    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    if errors > 0 {
        return Err(format!("docs-lint found {errors} error(s)").into());
    }
    Ok(())
}

/// Checks the prose fragments: they must parse, correspond to a registered node, and (when marked
/// `stable`) carry a Purpose and leave no parameter on the prettified fallback.
fn check_fragments(docs: &Docs, root: &Path, out: &mut Vec<Finding>) -> Result<(), Box<dyn Error>> {
    let fragment_dir = root.join("crates/ymir-nodes/docs");
    let type_ids: HashSet<&str> = docs.nodes.iter().map(|n| n.type_id.as_str()).collect();

    let mut fragments: HashMap<String, Fragment> = HashMap::new();
    for entry in std::fs::read_dir(&fragment_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some("README") | None => continue,
            Some(stem) => stem.to_string(),
        };
        let text = std::fs::read_to_string(&path)?;
        match fragment::parse(&text) {
            Ok(frag) => {
                if !type_ids.contains(stem.as_str()) {
                    out.push(Finding::error(format!(
                        "orphan fragment {}: no registered node has type_id `{stem}`",
                        path.display()
                    )));
                }
                fragments.insert(stem, frag);
            }
            Err(e) => out.push(Finding::error(format!("{}: {e}", path.display()))),
        }
    }

    for node in &docs.nodes {
        let Some(frag) = fragments.get(&node.type_id) else {
            out.push(Finding::gap(format!(
                "node `{}` has no prose fragment yet",
                node.type_id
            )));
            continue;
        };
        if frag.status.as_deref() != Some("stable") {
            continue;
        }
        if frag.section("Purpose").is_none() {
            out.push(Finding::error(format!(
                "stable node `{}` has an empty Purpose",
                node.type_id
            )));
        }
        for p in &node.params {
            if p.source == "prettified" {
                out.push(Finding::error(format!(
                    "stable node `{}` parameter `{}` has no catalog string (prettified fallback)",
                    node.type_id, p.name
                )));
            }
        }
    }
    Ok(())
}

/// Checks every published page under `docs/`: no forbidden markers, no broken intra-site links or
/// missing figures, and no relative link into `design/`.
fn check_published_pages(root: &Path, out: &mut Vec<Finding>) -> Result<(), Box<dyn Error>> {
    let docs_dir = root.join("docs");
    let mut pages = Vec::new();
    collect_markdown(&docs_dir, &mut pages)?;

    for page in &pages {
        let text = std::fs::read_to_string(page)?;
        let rel = page
            .strip_prefix(root)
            .unwrap_or(page)
            .display()
            .to_string();

        for marker in forbidden_markers(&text) {
            out.push(Finding::error(format!(
                "{rel}: forbidden marker {marker:?} in a published page"
            )));
        }

        let dir = page.parent().unwrap_or(root);
        for link in md_links(&text) {
            if link.target.starts_with("http://") || link.target.starts_with("https://") {
                continue;
            }
            if link.target.contains("design/") {
                out.push(Finding::error(format!(
                    "{rel}: relative link into design/ ({:?}); use an absolute GitHub URL",
                    link.target
                )));
                continue;
            }
            // The file part of a target, dropping any `#anchor`.
            let file_part = link.target.split('#').next().unwrap_or(&link.target);
            if file_part.is_empty() {
                continue; // a pure `#anchor` on the same page
            }
            let is_figure = link.is_image;
            let is_page_link = file_part.ends_with(".md");
            if (is_figure || is_page_link) && !dir.join(file_part).exists() {
                let kind = if is_figure {
                    "missing figure"
                } else {
                    "broken link"
                };
                out.push(Finding::error(format!("{rel}: {kind} {:?}", link.target)));
            }
        }
    }
    Ok(())
}

/// Collects every `.md` file under `dir`, recursively.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

/// The forbidden markers present in a text, case-insensitively.
fn forbidden_markers(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    FORBIDDEN_MARKERS
        .iter()
        .copied()
        .filter(|m| lower.contains(m))
        .collect()
}

/// A Markdown link or image reference and its target.
struct MdLink {
    is_image: bool,
    target: String,
}

/// Extracts `[label](target)` links and `![alt](target)` images. A small scanner, not a full
/// Markdown parser: it handles the inline links the reference and fragments actually use.
fn md_links(text: &str) -> Vec<MdLink> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(rel_close) = text[i..].find(']') else {
            break;
        };
        let close = i + rel_close;
        if close + 1 < bytes.len()
            && bytes[close + 1] == b'('
            && let Some(rel_rparen) = text[close + 2..].find(')')
        {
            let end = close + 2 + rel_rparen;
            out.push(MdLink {
                is_image: i > 0 && bytes[i - 1] == b'!',
                target: text[close + 2..end].to_string(),
            });
            i = end + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// Prints the findings, errors first, and a one-line summary.
fn report(findings: &[Finding]) {
    let errors: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect();
    let gaps = findings.len() - errors.len();

    for f in &errors {
        eprintln!("error: {}", f.message);
    }
    for f in findings.iter().filter(|f| f.severity == Severity::Gap) {
        eprintln!("gap: {}", f.message);
    }
    if errors.is_empty() {
        println!("docs-lint: clean ({gaps} gap(s) pending)");
    } else {
        eprintln!("docs-lint: {} error(s), {gaps} gap(s)", errors.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_markers_are_case_insensitive() {
        assert_eq!(forbidden_markers("A TODO here"), vec!["todo"]);
        assert_eq!(
            forbidden_markers("This is TBD and Proposed"),
            vec!["tbd", "proposed"]
        );
        assert!(forbidden_markers("An open question remains").contains(&"open question"));
        assert!(forbidden_markers("Clean prose about terrain.").is_empty());
    }

    #[test]
    fn md_links_extracts_links_and_images() {
        let text =
            "See [Blur](modifier.blur.md) and ![alt](images/x.png), plus [ext](https://a.b).";
        let links = md_links(text);
        assert_eq!(links.len(), 3);
        assert!(!links[0].is_image && links[0].target == "modifier.blur.md");
        assert!(links[1].is_image && links[1].target == "images/x.png");
        assert!(!links[2].is_image && links[2].target == "https://a.b");
    }

    #[test]
    fn md_links_ignores_a_bracket_without_a_target() {
        assert!(md_links("a [reference] with no link").is_empty());
    }

    #[test]
    fn check_published_pages_flags_markers_links_and_design_refs() {
        let root = std::env::temp_dir().join(format!("ymir-lint-{}", std::process::id()));
        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).expect("temp docs dir");
        std::fs::write(
            docs.join("a.md"),
            "A TODO here. A [dead](missing.md) link, an [ok](a.md) link, a \
             [design](../design/x.md) ref, and an [ext](https://example.com) link.",
        )
        .expect("write page");

        let mut findings = Vec::new();
        check_published_pages(&root, &mut findings).expect("check runs");
        // shortcut-ok: best-effort temp-dir cleanup; a failed remove must not fail the test
        std::fs::remove_dir_all(&root).ok();

        let msgs: Vec<&str> = findings.iter().map(|f| f.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("forbidden marker")),
            "should flag TODO: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("broken link") && m.contains("missing.md")),
            "should flag the dead link: {msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("into design/")),
            "should flag the design/ ref: {msgs:?}"
        );
        // The existing sibling and the external link must not be flagged.
        assert!(
            !msgs
                .iter()
                .any(|m| m.contains("\"a.md\"") || m.contains("example.com")),
            "valid links must pass: {msgs:?}"
        );
    }
}
