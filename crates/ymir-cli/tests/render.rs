//! End-to-end tests for `ymir-cli render` (#30), driving the real binary.
//!
//! The unit tests in `render.rs` cover argument parsing. These cover the part that only shows up
//! when the whole thing runs: that a project file written to disk produces a file on disk, that
//! the world settings in the file are what the render uses, and that a mistake exits non-zero
//! rather than reporting success and writing nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A minimal version 2 project: one fBm generator, no export node, at a distinctive world.
///
/// Written as literal JSON rather than built through the API, so it doubles as a check that the
/// documented file shape is the shape the binary actually reads. A change that breaks it should
/// be a deliberate format decision, not a silent one.
const PROJECT: &str = r#"{
  "format_version": 2,
  "world": {
    "seed": 7,
    "world_extent": 2048.0,
    "world_height": 512.0,
    "sea_level": 0.25,
    "build_res": 64
  },
  "graph": {
    "format_version": 1,
    "next_stable_id": 1,
    "nodes": [ { "stable_id": 0, "type_id": "generator.fbm" } ]
  }
}"#;

/// A scratch directory unique to one test, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ymir-cli-render-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes `PROJECT` into the scratch directory and returns its path.
    fn project(&self) -> PathBuf {
        let path = self.path("project.ymir");
        std::fs::write(&path, PROJECT).expect("write project");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover temp directory is not worth failing a passing test over, and a
        // failing one is easier to diagnose with its files still there.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the binary under test with `args`.
fn ymir(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ymir-cli"))
        .args(args)
        .output()
        .expect("run ymir-cli")
}

/// Everything the run printed, for a failure message that shows what actually happened.
fn transcript(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The pixel dimensions of a PNG, read from its IHDR header.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).expect("read png");
    assert!(bytes.len() > 24, "not a PNG: {} bytes", bytes.len());
    assert_eq!(&bytes[1..4], b"PNG", "not a PNG");
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (read(16), read(20))
}

#[test]
fn a_project_renders_to_the_file_it_was_told_to() {
    let scratch = Scratch::new("basic");
    let project = scratch.project();
    let out = scratch.path("height.png");

    let run = ymir(&[
        "render",
        project.to_str().expect("utf-8 path"),
        "--out",
        out.to_str().expect("utf-8 path"),
    ]);
    assert!(run.status.success(), "{}", transcript(&run));
    assert!(out.exists(), "no file written\n{}", transcript(&run));
    assert_eq!(
        png_size(&out),
        (64, 64),
        "the project's own build_res is what a render with no --res uses"
    );
}

#[test]
fn res_overrides_the_projects_build_resolution() {
    let scratch = Scratch::new("res");
    let project = scratch.project();
    let out = scratch.path("height.png");

    let run = ymir(&[
        "render",
        project.to_str().expect("utf-8 path"),
        "--res",
        "32",
        "--out",
        out.to_str().expect("utf-8 path"),
    ]);
    assert!(run.status.success(), "{}", transcript(&run));
    assert_eq!(png_size(&out), (32, 32));
}

#[test]
fn the_same_project_renders_the_same_bytes_twice() {
    // Same-machine repeatability, the rung the determinism contract actually requires. A headless
    // render is where someone would notice it failing, since a script reruns the same command.
    let scratch = Scratch::new("repeat");
    let project = scratch.project();
    let first = scratch.path("first.png");
    let second = scratch.path("second.png");

    for out in [&first, &second] {
        let run = ymir(&[
            "render",
            project.to_str().expect("utf-8 path"),
            "--out",
            out.to_str().expect("utf-8 path"),
        ]);
        assert!(run.status.success(), "{}", transcript(&run));
    }
    assert_eq!(
        std::fs::read(&first).expect("read first"),
        std::fs::read(&second).expect("read second"),
        "two renders of one project differ"
    );
}

#[test]
fn every_output_format_writes_something_readable() {
    let scratch = Scratch::new("formats");
    let project = scratch.project();
    // 64 cells: a PNG is 2 bytes a cell and an R16 the same, so both land near 8 KiB before
    // compression. The assertion is only that each wrote a plausible file, not its exact size.
    for name in ["h.png", "h.r16", "h.exr"] {
        let out = scratch.path(name);
        let run = ymir(&[
            "render",
            project.to_str().expect("utf-8 path"),
            "--out",
            out.to_str().expect("utf-8 path"),
        ]);
        assert!(run.status.success(), "{name}: {}", transcript(&run));
        let size = std::fs::metadata(&out).expect("stat output").len();
        assert!(size > 100, "{name} is {size} bytes, which is not an image");
    }
    assert_eq!(
        std::fs::read(scratch.path("h.r16"))
            .expect("read r16")
            .len(),
        64 * 64 * 2,
        "r16 is raw 16-bit samples, so its size is exactly the cell count"
    );
}

#[test]
fn a_missing_project_fails_loudly_and_names_the_file() {
    let run = ymir(&["render", "definitely-not-here.ymir", "--out", "x.png"]);
    assert!(!run.status.success(), "{}", transcript(&run));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("definitely-not-here.ymir"),
        "the message must name the file that was not found: {stderr}"
    );
    assert!(
        !Path::new("x.png").exists(),
        "a failed render must not leave a file behind"
    );
}

#[test]
fn a_project_ending_in_an_ordinary_node_needs_somewhere_to_write() {
    // The common case for a project authored in the editor: it ends in whatever was being
    // previewed, so there is no path in the file. Succeeding silently here would be the worst
    // outcome, since nothing would be written and the exit status would say it worked.
    let scratch = Scratch::new("no-out");
    let project = scratch.project();

    let run = ymir(&["render", project.to_str().expect("utf-8 path")]);
    assert!(!run.status.success(), "{}", transcript(&run));
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("--out"),
        "the message must say what to do: {}",
        transcript(&run)
    );
}

#[test]
fn an_older_project_is_told_its_version_rather_than_half_read() {
    let scratch = Scratch::new("v1");
    let project = scratch.path("old.ymir");
    std::fs::write(
        &project,
        PROJECT.replace("\"format_version\": 2", "\"format_version\": 1"),
    )
    .expect("write v1 project");

    let run = ymir(&[
        "render",
        project.to_str().expect("utf-8 path"),
        "--out",
        scratch.path("x.png").to_str().expect("utf-8 path"),
    ]);
    assert!(!run.status.success(), "{}", transcript(&run));
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("version"),
        "the message must say the version is the problem: {stderr}"
    );
}
