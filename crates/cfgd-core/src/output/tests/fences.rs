//! Source-shaped fences: two invariants of `renderer/` that no runtime
//! assertion can hold, because both are about code that must not exist.

use std::path::{Path, PathBuf};

/// Workspace root. `CARGO_MANIFEST_DIR` = `<root>/crates/cfgd-core`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Every `.rs` file under every crate's `src/`.
fn workspace_rust_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![workspace_root().join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    assert!(!out.is_empty(), "found no sources under crates/");
    out
}

/// `MultiProgress::suspend` and `ProgressBar::suspend` both `unwrap()` an
/// `io::Result` internally, so a terminal that goes away during a suspended
/// write aborts the process. `emit_block`'s `println` route returns its error
/// instead, which is why the latch can exist at all. The one API that could
/// have re-introduced the call from outside `output/` — `Printer::multi_progress()`
/// — was deleted with this fence.
#[test]
fn suspend_is_never_called() {
    let mut offenders = Vec::new();
    for path in workspace_rust_files() {
        if path.ends_with(Path::new("output/tests/fences.rs")) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in body.lines().enumerate() {
            if line.contains(".suspend(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "indicatif's suspend() unwraps io errors; route through the renderer's \
         emit_block instead:\n{}",
        offenders.join("\n")
    );
}

/// Extract the body of every `struct Emitting` / `impl … Emitting` region in
/// `source`, by brace matching from the region's opening `{`.
fn emitting_regions(source: &str) -> Vec<String> {
    let mut regions = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (start, line) in lines.iter().enumerate() {
        let opens_region = line.contains("struct Emitting")
            || (line.trim_start().starts_with("impl") && line.contains("Emitting"));
        if !opens_region {
            continue;
        }
        let mut depth = 0usize;
        let mut seen_open = false;
        let mut body = String::new();
        for line in &lines[start..] {
            body.push_str(line);
            body.push('\n');
            for c in line.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        seen_open = true;
                    }
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if seen_open && depth == 0 {
                break;
            }
        }
        regions.push(body);
    }
    regions
}

/// The collector split is what makes the deferred-header flush and the kv
/// drain unable to re-enter `write_line` — they hold `&mut RenderState`, not a
/// sink and not the lock. A collector that regained either would deadlock or
/// emit out of band, and neither failure is visible in a diff.
#[test]
fn emit_collectors_take_no_sink() {
    let renderer = workspace_root().join("crates/cfgd-core/src/output/renderer");
    let files = [
        renderer.join("mod.rs"),
        renderer.join("kv.rs"),
        renderer.join("section.rs"),
        renderer.join("status.rs"),
        renderer.join("table.rs"),
    ];
    let banned = ["Writer", "self.state.lock(", "write_line("];
    let mut regions = Vec::new();
    let mut offenders = Vec::new();
    for path in files {
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        for region in emitting_regions(&body) {
            for needle in banned {
                if region.contains(needle) {
                    offenders.push(format!(
                        "{}: collector region mentions {needle}",
                        path.display()
                    ));
                }
            }
            regions.push(region);
        }
    }
    // The struct plus its three impls (mod.rs, kv.rs, section.rs). A lower
    // count means the extractor stopped matching and the fence proves nothing.
    assert!(
        regions.len() >= 4,
        "matched only {} Emitting regions",
        regions.len()
    );
    assert!(
        regions
            .iter()
            .any(|r| r.contains("fn push_line") && r.contains("fn drain_kv_buffer")),
        "the extracted regions are truncated — the collector bodies are missing"
    );
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}
