//! Guards that every full cfgd-resource YAML example in `docs/**` validates
//! against the live schema registry.
//!
//! Walks the repo docs tree, extracts each fenced ```yaml block whose body is a
//! real cfgd resource (begins with `apiVersion: cfgd.io/v1alpha1` and carries
//! concrete values, not schema-sketch placeholders like `name: string`), and
//! runs it through `validate_document`. A malformed example (wrong field shape,
//! bare-string ref where an object is required, stray placeholder) fails loudly.
//!
//! Gated on the `crd` feature: many doc examples are CRD kinds (MachineConfig,
//! ConfigPolicy, DriftAlert), whose registry entries exist only when `crd` is
//! enabled. The default build enables it, so this runs in the normal test pass;
//! the CSI-style crd-off build skips it rather than reporting false failures for
//! kinds it cannot resolve.

#![cfg(feature = "crd")]

use std::path::{Path, PathBuf};

use cfgd_core::generate::validate::validate_document;

/// Recursively collect every `.md` file under `dir`.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => panic!("cannot read docs dir {}: {e}", dir.display()),
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

/// A fenced ```yaml block paired with its source location for diagnostics.
struct YamlBlock {
    file: PathBuf,
    /// 1-based line number of the opening fence.
    line: usize,
    body: String,
}

/// Extract every ```yaml (or ```yml) fenced block from `text`.
fn yaml_blocks(file: &Path, text: &str) -> Vec<YamlBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed == "```yaml" || trimmed == "```yml" {
            let fence_line = i + 1;
            let mut body = String::new();
            i += 1;
            while i < lines.len() && lines[i].trim_start() != "```" {
                body.push_str(lines[i]);
                body.push('\n');
                i += 1;
            }
            blocks.push(YamlBlock {
                file: file.to_path_buf(),
                line: fence_line,
                body,
            });
        }
        i += 1;
    }
    blocks
}

/// A schema-sketch fragment uses placeholder type tokens as scalar values
/// (`field: string`, `field: int`, a `A | B` union) or OpenAPI CRD-schema keys
/// (`x-kubernetes-*`) rather than concrete values. Such blocks describe a shape,
/// not a resource, so they are not validated as documents.
fn is_schema_sketch(body: &str) -> bool {
    body.lines().any(|raw| {
        let line = raw.trim();
        if line.contains("x-kubernetes-") {
            return true;
        }
        let Some((_, value)) = line.split_once(": ") else {
            return false;
        };
        let value = value.trim();
        matches!(value, "string" | "int" | "bool" | "integer" | "boolean")
            || (value.contains(" | ") && value.contains("string"))
    })
}

/// A real cfgd resource block begins with the cfgd `apiVersion` and is not a
/// schema sketch.
fn is_cfgd_resource(body: &str) -> bool {
    body.trim_start()
        .starts_with("apiVersion: cfgd.io/v1alpha1")
        && !is_schema_sketch(body)
}

/// Kinds that carry the cfgd `apiVersion` but are owned by another control plane
/// (Crossplane composite types), so cfgd's registry does not validate them.
/// Listed explicitly so a typo'd cfgd kind still fails rather than being skipped.
const EXTERNAL_KINDS: &[&str] = &["TeamConfig"];

/// The `kind:` value of a resource block, if present.
fn block_kind(body: &str) -> Option<&str> {
    body.lines().find_map(|line| {
        line.trim()
            .strip_prefix("kind:")
            .map(str::trim)
            .filter(|k| !k.is_empty())
    })
}

#[test]
fn every_docs_resource_example_validates() {
    let docs_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs"));
    assert!(
        docs_dir.is_dir(),
        "docs dir not found at {}",
        docs_dir.display()
    );

    let mut files = Vec::new();
    markdown_files(&docs_dir, &mut files);
    files.sort();

    let mut validated = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        for block in yaml_blocks(file, &text) {
            if !is_cfgd_resource(&block.body) {
                continue;
            }
            if block_kind(&block.body).is_some_and(|k| EXTERNAL_KINDS.contains(&k)) {
                continue;
            }
            let result = validate_document(&block.body);
            assert!(
                result.valid,
                "{}:{} — cfgd resource example failed validation: {}",
                block.file.display(),
                block.line,
                result
                    .errors
                    .first()
                    .map(String::as_str)
                    .unwrap_or("(no error message)")
            );
            validated += 1;
        }
    }

    // A zero (or near-zero) count means the extractor silently stopped matching
    // real blocks — a guard that proves nothing. Pin a sane floor.
    assert!(
        validated >= 5,
        "expected to validate at least 5 docs resource examples, found {validated}"
    );
}
