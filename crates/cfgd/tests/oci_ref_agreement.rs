//! Cross-crate guard: `cfgd_crd::is_valid_oci_reference` must agree with the
//! full `cfgd_core::oci::OciReference` parser on every input.
//!
//! `cfgd-crd` sits beneath `cfgd-core` in the dependency graph, so it cannot
//! call the real parser and instead reimplements the parser's reject rules as a
//! standalone predicate. This test — in the only crate that depends on both —
//! pins the two to the same accept/reject verdict so the predicate cannot drift
//! away from the parser undetected.

#[test]
fn predicate_agrees_with_parser_across_corpus() {
    let corpus = [
        // --- original corpus ---
        "",
        "has space",
        "\u{7f}ctl",
        "localhost:5000",
        "foo:1234",
        "localhost:5000/repo",
        "foo:1234/bar",
        "registry.example.com/repo:v1",
        "ghcr.io/org/mod@sha256:abcd",
        "myrepo",
        "org/repo",
        "registry.example.com/",
        // --- multi-segment repo paths ---
        "registry.example.com/org/team/sub/repo:v1",
        "org/team/sub/repo",
        "registry.example.com/org/team/sub/repo@sha256:abcd",
        // --- uppercase in various positions ---
        "Registry.Example.Com/repo:v1",
        "registry.example.com/Org/Repo:v1",
        "registry.example.com/repo:V1-RC1",
        "MYREPO",
        "ghcr.io/ORG/MOD:LATEST",
        // --- digest-only refs (no tag) ---
        "repo@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "ghcr.io/org/mod@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "repo@sha256:xyz",
        // --- tag + digest combined ---
        "repo:v1@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "ghcr.io/org/mod:v1@sha256:abcd",
        // --- port edge cases ---
        "host:99999/repo",
        "host:/repo",
        "host:0/repo",
        "host.io:5000/repo:v1",
        "localhost:0",
        // --- leading / trailing / double slashes ---
        "/repo",
        "org//repo",
        "repo/",
        "//repo",
        "registry.example.com//repo",
        // --- empty tag / empty digest / empty name ---
        "repo:",
        "repo@",
        "@sha256:abcd",
        ":v1",
        "@",
        ":",
        // --- boundary-length names ---
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        // --- whitespace / control-char variants ---
        "\trepo",
        "repo\n",
        "re\u{0}po",
        "repo:v1 ",
        " ghcr.io/org/mod:v1",
        "ghcr.io/org\u{200b}/mod",
    ];

    for s in corpus {
        let predicate = cfgd_crd::is_valid_oci_reference(s);
        let parser = cfgd_core::oci::OciReference::parse(s).is_ok();
        assert_eq!(
            predicate, parser,
            "cfgd_crd::is_valid_oci_reference({s:?}) = {predicate} but \
             cfgd_core::oci::OciReference::parse({s:?}).is_ok() = {parser} — \
             the predicate has drifted from the parser"
        );
    }
}
