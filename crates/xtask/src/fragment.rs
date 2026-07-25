//! The optional per-node prose fragment (`ymir-nodes/docs/<type_id>.md`) that merges into a
//! generated reference page.
//!
//! A fragment is authoring input, not a finished page: leading frontmatter carries the node's
//! `status` and an optional `resolution_dependent` flag (resolution dependence is a physical
//! property with no home in `NodeSpec`, so it is authored here), and the body holds a fixed set of
//! `## ` sections that the generator drops into the page in place of the mechanical ones.

use std::collections::BTreeMap;
use std::error::Error;

/// The section headings a fragment may carry, in page order. Any other heading is an authoring
/// error, so a typo cannot silently drop prose.
pub(crate) const SECTIONS: [&str; 4] = ["Purpose", "Behaviour", "Recipes", "See also"];

/// A parsed fragment: its frontmatter flags and its prose sections keyed by heading.
#[derive(Debug, Default)]
pub(crate) struct Fragment {
    /// The authored `status` (`draft` or `stable`), or `None` when the fragment omits it.
    pub status: Option<String>,
    /// Whether the node is an iterative simulation whose result changes with resolution.
    pub resolution_dependent: bool,
    /// Prose section bodies keyed by heading (one of [`SECTIONS`]).
    pub sections: BTreeMap<String, String>,
}

impl Fragment {
    /// The prose for a section heading, if the fragment carries it and it is non-empty.
    pub fn section(&self, heading: &str) -> Option<&str> {
        self.sections
            .get(heading)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

/// Parses a fragment. A leading `---` frontmatter block carries `status` and
/// `resolution_dependent`; the remaining body is split into `## Heading` sections. An unterminated
/// frontmatter, an unparseable frontmatter line, an unknown key, or an unknown heading is an error.
pub(crate) fn parse(text: &str) -> Result<Fragment, Box<dyn Error>> {
    let mut frag = Fragment::default();

    let body = match text.strip_prefix("---\n") {
        Some(after_open) => {
            let close = after_open
                .find("\n---")
                .ok_or("unterminated frontmatter (no closing `---`)")?;
            parse_frontmatter(&after_open[..close], &mut frag)?;
            after_open[close + "\n---".len()..].trim_start_matches('\n')
        }
        None => text,
    };

    parse_sections(body, &mut frag)?;
    Ok(frag)
}

/// Reads the `key: value` frontmatter lines into the fragment.
fn parse_frontmatter(front: &str, frag: &mut Fragment) -> Result<(), Box<dyn Error>> {
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("frontmatter line is not `key: value`: {line:?}"))?;
        let value = value.trim();
        match key.trim() {
            "status" => frag.status = Some(value.to_string()),
            "resolution_dependent" => frag.resolution_dependent = value == "true",
            // The generator sets the page title from the display name; an authored `title` is
            // tolerated so a fragment reads as a normal page, but it is ignored.
            "title" => {}
            other => return Err(format!("unknown frontmatter key {other:?}").into()),
        }
    }
    Ok(())
}

/// Splits the body into `## Heading` sections, validating each heading against [`SECTIONS`].
fn parse_sections(body: &str, frag: &mut Fragment) -> Result<(), Box<dyn Error>> {
    let mut current: Option<String> = None;
    let mut buf = String::new();
    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(h) = current.take() {
                frag.sections.insert(h, buf.trim().to_string());
                buf.clear();
            }
            let heading = heading.trim();
            if !SECTIONS.contains(&heading) {
                return Err(
                    format!("unknown section heading {heading:?} (allowed: {SECTIONS:?})").into(),
                );
            }
            current = Some(heading.to_string());
        } else if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(h) = current.take() {
        frag.sections.insert(h, buf.trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_sections() {
        let text = "---\nstatus: stable\nresolution_dependent: true\n---\n\n## Purpose\n\nCarves the terrain.\n\n## See also\n\n- [Hydraulic](hydraulic.md)\n";
        let f = parse(text).expect("parses");
        assert_eq!(f.status.as_deref(), Some("stable"));
        assert!(f.resolution_dependent);
        assert_eq!(f.section("Purpose"), Some("Carves the terrain."));
        assert!(f.section("See also").unwrap().contains("Hydraulic"));
        assert!(f.section("Behaviour").is_none());
    }

    #[test]
    fn a_bodyless_fragment_is_fine() {
        let f = parse("---\nstatus: draft\n---\n").expect("parses");
        assert_eq!(f.status.as_deref(), Some("draft"));
        assert!(!f.resolution_dependent);
        assert!(f.sections.is_empty());
    }

    #[test]
    fn an_unknown_heading_is_an_error() {
        let err = parse("## Wat\n\nNope.\n").expect_err("unknown heading rejected");
        assert!(err.to_string().contains("Wat"));
    }

    #[test]
    fn an_unknown_frontmatter_key_is_an_error() {
        let err = parse("---\nwidth: 5\n---\n").expect_err("unknown key rejected");
        assert!(err.to_string().contains("width"));
    }
}
