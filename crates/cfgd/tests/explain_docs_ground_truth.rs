//! Ground-truth guard for `explain`'s embedded field-heading lookup
//! (`crates/cfgd/src/cli/explain/mod.rs::doc_body`): its `docs-spec/`
//! fixtures must match their canonical `docs/spec/` sources byte-for-byte.
//!
//! The fixtures live inside the crate (`tests/fixtures/docs-spec/`) rather
//! than pointing `include_str!` at the workspace root, because `cargo
//! package` builds this crate in isolation and a path reaching outside the
//! crate directory does not exist in that tree — the same reason
//! `cfgd-core`'s `skill_model` embeds crate-local copies of `examples/**`
//! (see `crates/cfgd-core/tests/skill_examples_ground_truth.rs`).

const FILES: &[&str] = &[
    "module.md",
    "profile.md",
    "config.md",
    "machineconfig.md",
    "configpolicy.md",
    "clusterconfigpolicy.md",
    "driftalert.md",
    "teamconfig.md",
];

#[test]
fn embedded_doc_bodies_match_workspace_docs() {
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_docs = crate_root.join("../../docs/spec");
    if !workspace_docs.is_dir() {
        // Published-tarball context: the workspace root isn't shipped, so
        // there is no canonical copy to compare against.
        return;
    }
    for name in FILES {
        let fixture = crate_root.join("tests/fixtures/docs-spec").join(name);
        let canonical = workspace_docs.join(name);
        let fixture_body = std::fs::read_to_string(&fixture)
            .unwrap_or_else(|e| panic!("fixture {} unreadable: {e}", fixture.display()));
        let canonical_body = std::fs::read_to_string(&canonical)
            .unwrap_or_else(|e| panic!("canonical {} unreadable: {e}", canonical.display()));
        assert_eq!(
            fixture_body, canonical_body,
            "crate fixture drifted from docs/spec/{name}; re-copy it: \
             cp docs/spec/{name} crates/cfgd/tests/fixtures/docs-spec/{name}"
        );
    }
}
